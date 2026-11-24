use std::collections::VecDeque;
use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex, Weak};

use reviewq_app::access::AppError;
use reviewq_domain::identity::MrIdentity;
use tokio::sync::{Notify, mpsc, watch};

use crate::http::{Response, ResponseReader};

use super::GitLab;

const MAX_FLIGHTS: usize = 4;
const MAX_SUBSCRIBERS: usize = 4;
const CHUNK_BYTES: usize = 32 * 1024;
const REPLAY_CHUNKS: usize = 4;

#[derive(Clone, Default)]
pub(super) struct Flights(Arc<Mutex<Vec<FlightEntry>>>);

struct FlightEntry {
    key: PageKey,
    flight: Weak<Flight>,
}

#[derive(PartialEq, Eq)]
struct PageKey {
    source: MrIdentity,
    revision: String,
    page: u32,
    limit: u16,
}

type Chunk = Arc<[u8]>;
type End = Result<(Result<(), AppError>, Option<u32>), AppError>;

struct Flight {
    state: Mutex<FlightState>,
    idle: Notify,
    ended: watch::Sender<Option<End>>,
}

struct FlightState {
    next_id: u64,
    readers: Vec<(u64, mpsc::Sender<Chunk>)>,
    replay: Option<VecDeque<Chunk>>,
}

struct Listener {
    _interest: Interest,
    input: Input,
    ended: watch::Receiver<Option<End>>,
}

struct Joined {
    listener: Listener,
    new_flight: Option<Arc<Flight>>,
}

struct Interest {
    flight: Arc<Flight>,
    id: u64,
}

struct Input {
    current: Cursor<Chunk>,
    replay: VecDeque<Chunk>,
    incoming: mpsc::Receiver<Chunk>,
}

impl GitLab {
    pub(super) async fn decode_diff_page<T: Send + 'static>(
        &self,
        source: &MrIdentity,
        revision: &str,
        page: u32,
        limit: u16,
        decode: impl FnOnce(ResponseReader) -> Result<T, AppError> + Send + 'static,
    ) -> Result<(Result<T, AppError>, Option<u32>), AppError> {
        let key = PageKey {
            source: source.clone(),
            revision: revision.to_owned(),
            page,
            limit,
        };
        if let Some(joined) = self.patches.flights.join(key)? {
            if let Some(flight) = joined.new_flight {
                let provider = self.clone();
                let source = source.clone();
                tokio::spawn(async move {
                    flight
                        .run(async { provider.receive_diff_page(&source, page, limit).await })
                        .await;
                });
            }
            return joined.listener.decode(decode).await;
        }
        // A late subscriber outside the replay prefix makes a fresh bounded read.
        let (response, next) = self.receive_diff_page(source, page, limit).await?;
        Ok((response.decode(decode).await, next))
    }
}

impl Flights {
    fn join(&self, key: PageKey) -> Result<Option<Joined>, AppError> {
        let mut flights = self.0.lock().map_err(|_| AppError::Internal)?;
        flights.retain(|entry| entry.flight.strong_count() != 0);
        if let Some(flight) = flights
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.flight.upgrade())
        {
            return Ok(flight.listen()?.map(|listener| Joined {
                listener,
                new_flight: None,
            }));
        }
        if flights.len() == MAX_FLIGHTS {
            return Ok(None);
        }
        let flight = Arc::new(Flight {
            state: Mutex::new(FlightState {
                next_id: 1,
                readers: Vec::new(),
                replay: Some(VecDeque::new()),
            }),
            idle: Notify::new(),
            ended: watch::channel(None).0,
        });
        let listener = flight.listen()?.ok_or(AppError::Internal)?;
        flights.push(FlightEntry {
            key,
            flight: Arc::downgrade(&flight),
        });
        Ok(Some(Joined {
            listener,
            new_flight: Some(flight),
        }))
    }
}

impl Flight {
    fn listen(self: &Arc<Self>) -> Result<Option<Listener>, AppError> {
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        let Some(replay) = state.replay.clone() else {
            return Ok(None);
        };
        if state.readers.len() == MAX_SUBSCRIBERS {
            return Ok(None);
        }
        let id = state.next_id;
        state.next_id += 1;
        let (sender, incoming) = mpsc::channel(1);
        state.readers.push((id, sender));
        Ok(Some(Listener {
            _interest: Interest {
                flight: self.clone(),
                id,
            },
            input: Input {
                current: Cursor::new(Arc::default()),
                replay,
                incoming,
            },
            ended: self.ended.subscribe(),
        }))
    }

    async fn run(
        self: Arc<Self>,
        open: impl Future<Output = Result<(Response, Option<u32>), AppError>>,
    ) {
        let end = tokio::select! {
            biased;
            _ = self.idle.notified() => Err(AppError::Unavailable),
            end = async {
                let (response, next) = open.await?;
                Ok((self.pump(response).await, next))
            } => end,
        };
        // Publish the classified transport outcome before the decoders see EOF.
        self.ended.send_replace(Some(end));
        if let Ok(mut state) = self.state.lock() {
            state.replay = None;
            state.readers.clear();
        }
    }

    async fn pump(&self, mut response: Response) -> Result<(), AppError> {
        response.check_body_length()?;
        while let Some(chunk) = response.chunk().await? {
            for bytes in chunk.chunks(CHUNK_BYTES) {
                let chunk = Arc::<[u8]>::from(bytes);
                for sender in self.publish(&chunk)? {
                    let _ = sender.send(chunk.clone()).await;
                }
            }
        }
        Ok(())
    }

    fn publish(&self, chunk: &Chunk) -> Result<Vec<mpsc::Sender<Chunk>>, AppError> {
        let mut state = self.state.lock().map_err(|_| AppError::Internal)?;
        if let Some(replay) = &mut state.replay {
            if replay.len() < REPLAY_CHUNKS {
                replay.push_back(chunk.clone());
            } else {
                state.replay = None;
            }
        }
        Ok(state
            .readers
            .iter()
            .map(|(_, sender)| sender.clone())
            .collect())
    }
}

impl Listener {
    async fn decode<T: Send + 'static>(
        self,
        decode: impl FnOnce(ResponseReader) -> Result<T, AppError> + Send + 'static,
    ) -> Result<(Result<T, AppError>, Option<u32>), AppError> {
        let Self {
            _interest,
            input,
            mut ended,
        } = self;
        let value = tokio::task::spawn_blocking(move || decode(Box::new(input)))
            .await
            .map_err(|_| AppError::Unavailable)?;
        if let Some(end) = ended.borrow().clone() {
            return with_end(value, end);
        }
        // A local parser failure can detach without interrupting the other readers.
        if let Err(error) = value {
            return Err(error);
        }
        ended.changed().await.map_err(|_| AppError::Unavailable)?;
        let end = ended.borrow().clone().ok_or(AppError::Internal)?;
        with_end(value, end)
    }
}

impl Drop for Interest {
    fn drop(&mut self) {
        if let Ok(mut state) = self.flight.state.lock() {
            state.readers.retain(|(id, _)| *id != self.id);
            if state.readers.is_empty() {
                state.replay = None;
                self.flight.idle.notify_one();
            }
        }
    }
}

impl Read for Input {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.current.read(bytes)?;
        if read != 0 || bytes.is_empty() {
            return Ok(read);
        }
        let Some(next) = self
            .replay
            .pop_front()
            .or_else(|| self.incoming.blocking_recv())
        else {
            return Ok(0);
        };
        self.current = Cursor::new(next);
        self.current.read(bytes)
    }
}

fn with_end<T>(
    value: Result<T, AppError>,
    end: End,
) -> Result<(Result<T, AppError>, Option<u32>), AppError> {
    let (transport, next) = end?;
    Ok((transport.and(value), next))
}
