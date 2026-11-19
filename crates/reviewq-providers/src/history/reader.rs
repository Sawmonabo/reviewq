use std::io::{self, Read};

const NAME_BYTES: usize = 6 * 256;
const MAX_NESTING: usize = 128;

/// Rejects names before the JSON parser has to allocate them. JSON syntax and
/// decoded strings still belong to Struson.
pub(super) struct NameBudget<R> {
    inner: R,
    stack: Vec<Scope>,
    string: Option<StringState>,
}

enum Scope {
    Object { expects_name: bool },
    Array,
}

struct StringState {
    name_bytes: Option<usize>,
    escaped: bool,
}

impl<R: Read> NameBudget<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            stack: Vec::new(),
            string: None,
        }
    }

    fn check(&mut self, byte: u8) -> io::Result<()> {
        if let Some(string) = &mut self.string {
            if byte == b'"' && !string.escaped {
                self.string = None;
            } else {
                if let Some(length) = &mut string.name_bytes {
                    *length += 1;
                    if *length > NAME_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "native field name exceeds limit",
                        ));
                    }
                }
                string.escaped = byte == b'\\' && !string.escaped;
            }
            return Ok(());
        }
        match byte {
            b'"' => {
                self.string = Some(StringState {
                    name_bytes: matches!(
                        self.stack.last(),
                        Some(Scope::Object { expects_name: true })
                    )
                    .then_some(0),
                    escaped: false,
                })
            }
            b'{' => self.stack.push(Scope::Object { expects_name: true }),
            b'[' => self.stack.push(Scope::Array),
            b'}' | b']' => {
                self.stack.pop();
            }
            b':' | b',' => {
                if let Some(Scope::Object { expects_name }) = self.stack.last_mut() {
                    *expects_name = byte == b',';
                }
            }
            _ => {}
        }
        if self.stack.len() > MAX_NESTING {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native record nesting exceeds limit",
            ));
        }
        Ok(())
    }
}

impl<R: Read> Read for NameBudget<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let written = self.inner.read(output)?;
        for byte in &output[..written] {
            self.check(*byte)?;
        }
        Ok(written)
    }
}
