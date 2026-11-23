use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use reviewq_app::access::AppError;
use reviewq_app::notifications::{DesktopClaim, DesktopOutcome};
use reviewq_domain::identity::NotificationId;
use reviewq_protocol::notifications::desktop::{BrowserFrame, DesktopFrame};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::create_response_with_body;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, Role, WebSocketConfig};

use super::{ApiError, Entry, auth::Auth};

const MAX_CLIENTS: usize = 8;
const MAX_FRAME_BYTES: usize = 2048;
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const OFFER_TIMEOUT: Duration = Duration::from_secs(2);
const DISPLAY_TIMEOUT: Duration = Duration::from_secs(5);
const DISPLAY_UNKNOWN: &str = "browser_notification_acknowledgement_unavailable";
const PROOF_REJECTED: CloseCode = CloseCode::Library(4001);

type WebSocket = WebSocketStream<TokioIo<Upgraded>>;

/// A bounded subscription shared between the browser listener and the desktop worker.
#[derive(Clone)]
pub struct BrowserDesktop {
    slots: Arc<Semaphore>,
    clients: Arc<Mutex<Vec<mpsc::Sender<Offer>>>>,
}

impl Default for BrowserDesktop {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(MAX_CLIENTS)),
            clients: Arc::default(),
        }
    }
}

pub enum BrowserOffer {
    Unavailable,
    Declined,
    Accepted(BrowserDisplay),
}

/// Only an accepted offer can authorize display. Dropping it cancels the offer.
pub struct BrowserDisplay {
    authorize: oneshot::Sender<oneshot::Sender<DesktopOutcome>>,
}

struct Offer {
    notification: NotificationId,
    body: String,
    event_count: u16,
    accepted: oneshot::Sender<BrowserDisplay>,
}

struct Registration<'a> {
    clients: &'a Mutex<Vec<mpsc::Sender<Offer>>>,
    sender: mpsc::Sender<Offer>,
}

#[derive(Debug)]
enum SocketError {
    Disconnected,
    Policy(&'static str),
    ProofRejected,
    Timeout,
}

impl BrowserDesktop {
    /// Offers one exact claim to one authenticated, ready browser.
    pub async fn offer(&self, claim: &DesktopClaim) -> BrowserOffer {
        let candidate = self.clients.lock().ok().and_then(|clients| {
            clients
                .iter()
                .find_map(|sender| sender.clone().try_reserve_owned().ok())
        });
        let Some(candidate) = candidate else {
            return BrowserOffer::Unavailable;
        };
        let (accepted, decision) = oneshot::channel();
        candidate.send(Offer {
            notification: claim.notification.clone(),
            body: claim.body(),
            event_count: claim.event_count,
            accepted,
        });
        decision
            .await
            .map_or(BrowserOffer::Declined, BrowserOffer::Accepted)
    }

    fn register(&self, sender: mpsc::Sender<Offer>) -> Result<Registration<'_>, SocketError> {
        self.clients
            .lock()
            .map_err(|_| SocketError::Disconnected)?
            .push(sender.clone());
        Ok(Registration {
            clients: self.clients.as_ref(),
            sender,
        })
    }
}

impl BrowserDisplay {
    /// Once authorization reaches the socket, an unknown outcome suppresses fallback.
    pub async fn show(self) -> Option<DesktopOutcome> {
        let (outcome, result) = oneshot::channel();
        self.authorize.send(outcome).ok()?;
        Some(
            result
                .await
                .unwrap_or(DesktopOutcome::Uncertain(DISPLAY_UNKNOWN)),
        )
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Ok(mut clients) = self.clients.lock() {
            clients.retain(|sender| !sender.same_channel(&self.sender));
        }
    }
}

pub(super) async fn open(
    State(entry): State<Arc<Entry>>,
    mut request: Request,
) -> Result<Response, ApiError> {
    entry.auth.require_browser_origin(request.headers())?;
    let response =
        create_response_with_body(&request, Body::empty).map_err(|_| AppError::InvalidRequest)?;
    let upgrade = request
        .extensions_mut()
        .remove::<OnUpgrade>()
        .ok_or(AppError::InvalidRequest)?;
    let config = WebSocketConfig::default()
        .read_buffer_size(MAX_FRAME_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_FRAME_BYTES)
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let browser = entry.browser_desktop.clone();
    let slot = browser
        .as_ref()
        .and_then(|browser| browser.slots.clone().try_acquire_owned().ok());
    tokio::spawn(async move {
        let Ok(Ok(upgraded)) = timeout(HANDSHAKE_TIMEOUT, upgrade).await else {
            return;
        };
        let mut socket =
            WebSocket::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
        let Some(browser) = browser else {
            close(
                &mut socket,
                CloseCode::Policy,
                "Desktop notifications are disabled.",
            )
            .await;
            return;
        };
        let Some(_slot) = slot else {
            close(
                &mut socket,
                CloseCode::Again,
                "Browser notification capacity reached.",
            )
            .await;
            return;
        };
        let mut shutdown = entry.shutdown.clone();
        let result = tokio::select! {
            result = serve(&mut socket, &entry.auth, &browser) => Some(result),
            _ = shutdown.wait_for(|stop| *stop) => None,
        };
        match result {
            Some(Err(SocketError::Policy(reason))) => {
                close(&mut socket, CloseCode::Policy, reason).await
            }
            Some(Err(SocketError::ProofRejected)) => {
                close(&mut socket, PROOF_REJECTED, "Browser proof was rejected.").await;
            }
            Some(Err(SocketError::Timeout)) => {
                close(
                    &mut socket,
                    CloseCode::Again,
                    "Browser notification timed out.",
                )
                .await;
            }
            None => close(&mut socket, CloseCode::Away, "ReviewQ is stopping.").await,
            Some(Err(SocketError::Disconnected) | Ok(())) => {}
        }
    });
    Ok(response)
}

async fn serve(
    socket: &mut WebSocket,
    auth: &Auth,
    browser: &BrowserDesktop,
) -> Result<(), SocketError> {
    let BrowserFrame::Hello { csrf_token } = read_before(socket, HANDSHAKE_TIMEOUT).await? else {
        return Err(SocketError::Policy("Expected browser proof."));
    };
    if !auth.valid_browser_proof(&csrf_token) {
        return Err(SocketError::ProofRejected);
    }
    let (sender, mut offers) = mpsc::channel(1);
    let _registration = browser.register(sender)?;
    send(socket, DesktopFrame::Ready).await?;
    loop {
        let offer = tokio::select! {
            offer = offers.recv() => offer.ok_or(SocketError::Disconnected)?,
            message = read(socket) => {
                message?;
                return Err(SocketError::Policy("Unexpected browser notification frame."));
            }
        };
        deliver(socket, offer).await?;
    }
}

async fn deliver(socket: &mut WebSocket, offer: Offer) -> Result<(), SocketError> {
    let id = offer.notification.as_str();
    send(
        socket,
        DesktopFrame::Offer {
            notification_id: id,
            title: "ReviewQ",
            body: &offer.body,
            event_count: offer.event_count,
        },
    )
    .await?;
    match read_before(socket, OFFER_TIMEOUT).await? {
        BrowserFrame::Accept { notification_id } if notification_id.as_str() == id => {}
        BrowserFrame::Decline { notification_id } if notification_id.as_str() == id => {
            return Err(SocketError::Policy(
                "Browser declined notification delivery.",
            ));
        }
        _ => {
            return Err(SocketError::Policy(
                "Unexpected notification offer response.",
            ));
        }
    }
    let (authorize, permission) = oneshot::channel();
    let permission = if offer.accepted.send(BrowserDisplay { authorize }).is_ok() {
        tokio::select! {
            result = timeout(OFFER_TIMEOUT, permission) => result.map_err(|_| SocketError::Timeout)?.ok(),
            message = read(socket) => {
                message?;
                return Err(SocketError::Policy("Unexpected frame before display authorization."));
            }
        }
    } else {
        None
    };
    let Some(outcome) = permission.filter(|result| !result.is_closed()) else {
        return send(
            socket,
            DesktopFrame::Cancel {
                notification_id: id,
            },
        )
        .await;
    };
    let displayed = async {
        send(
            socket,
            DesktopFrame::Display {
                notification_id: id,
            },
        )
        .await?;
        match read_before(socket, DISPLAY_TIMEOUT).await? {
            BrowserFrame::Displayed { notification_id } if notification_id.as_str() == id => {
                Ok(true)
            }
            BrowserFrame::Failed { notification_id } if notification_id.as_str() == id => Ok(false),
            _ => Err(SocketError::Policy(
                "Unexpected notification display response.",
            )),
        }
    }
    .await;
    let _ = outcome.send(if matches!(displayed, Ok(true)) {
        DesktopOutcome::Delivered
    } else {
        DesktopOutcome::Uncertain(DISPLAY_UNKNOWN)
    });
    displayed.map(|_| ())
}

async fn read(socket: &mut WebSocket) -> Result<BrowserFrame, SocketError> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str())
                    .map_err(|_| SocketError::Policy("Invalid browser notification frame."));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                return Err(SocketError::Policy(
                    "Browser notification frames must be text.",
                ));
            }
            Some(Ok(Message::Close(_) | Message::Frame(_)) | Err(_)) | None => {
                return Err(SocketError::Disconnected);
            }
        }
    }
}

async fn read_before(
    socket: &mut WebSocket,
    deadline: Duration,
) -> Result<BrowserFrame, SocketError> {
    timeout(deadline, read(socket))
        .await
        .map_err(|_| SocketError::Timeout)?
}

async fn send(socket: &mut WebSocket, frame: DesktopFrame<'_>) -> Result<(), SocketError> {
    let text = serde_json::to_string(&frame).map_err(|_| SocketError::Disconnected)?;
    timeout(WRITE_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .map_err(|_| SocketError::Timeout)?
        .map_err(|_| SocketError::Disconnected)
}

async fn close(socket: &mut WebSocket, code: CloseCode, reason: &'static str) {
    let closing = Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }));
    let _ = timeout(WRITE_TIMEOUT, socket.send(closing)).await;
}

#[cfg(test)]
mod tests {
    use reviewq_app::notifications::Kind;

    use super::*;

    #[tokio::test]
    async fn browser_handover_allows_fallback_only_before_display_authorization() {
        let desktop = BrowserDesktop::default();
        let claim = DesktopClaim {
            id: "receipt".into(),
            fence: 1,
            notification: "notification".parse().unwrap(),
            review: "review".parse().unwrap(),
            run: None,
            session: None,
            kind: Kind::Reply,
            label: "Review\tupdated".into(),
            event_count: 3,
        };
        assert!(matches!(
            desktop.offer(&claim).await,
            BrowserOffer::Unavailable
        ));

        let (first, mut first_offers) = mpsc::channel(1);
        let first_registration = desktop.register(first).unwrap();
        let (second, mut second_offers) = mpsc::channel(1);
        let _second_registration = desktop.register(second).unwrap();
        let (response, ()) = tokio::join!(desktop.offer(&claim), async {
            let offer = first_offers.recv().await.unwrap();
            assert_eq!(offer.notification, claim.notification);
            assert_eq!(offer.body, "Reviewupdated\n3 updates");
            assert!(second_offers.try_recv().is_err());
            drop(offer);
        });
        assert!(matches!(response, BrowserOffer::Declined));
        drop(first_registration);

        let (response, ()) = tokio::join!(desktop.offer(&claim), async {
            let offer = second_offers.recv().await.unwrap();
            let (authorize, permission) = oneshot::channel();
            assert!(offer.accepted.send(BrowserDisplay { authorize }).is_ok());
            drop(permission);
        });
        let BrowserOffer::Accepted(display) = response else {
            panic!("the successor browser did not receive the notification");
        };
        assert!(display.show().await.is_none());

        let (result, ()) = tokio::join!(
            async {
                let BrowserOffer::Accepted(display) = desktop.offer(&claim).await else {
                    panic!("the browser did not accept the notification");
                };
                display.show().await
            },
            async {
                let offer = second_offers.recv().await.unwrap();
                let (authorize, permission) = oneshot::channel();
                assert!(offer.accepted.send(BrowserDisplay { authorize }).is_ok());
                drop(permission.await.unwrap());
            },
        );
        assert!(matches!(
            result,
            Some(DesktopOutcome::Uncertain(DISPLAY_UNKNOWN))
        ));
    }
}
