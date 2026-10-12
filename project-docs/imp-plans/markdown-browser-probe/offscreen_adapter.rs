use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MESSAGES: usize = 32;
pub const ORIGIN: &str = "https://lathe-probe.invalid";

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Message {
    Hello {
        session: String,
        request: u32,
    },
    Acknowledged {
        session: String,
        request: u32,
    },
    Input {
        session: String,
        request: u32,
        text: String,
    },
    Event {
        session: String,
        request: u32,
        event: EventKind,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Pointer,
    Wheel,
    KeyUp,
}

pub fn document_url(session: &str) -> String {
    format!("{ORIGIN}/index.html?session={session}")
}

pub fn message(origin: &str, body: &str, session: &str) -> Result<Message> {
    if origin != document_url(session) || body.len() > 4096 {
        bail!("foreign or oversized bridge message");
    }
    let message: Message = serde_json::from_str(body)?;
    let actual = match &message {
        Message::Hello { session, .. }
        | Message::Acknowledged { session, .. }
        | Message::Input { session, .. }
        | Message::Event { session, .. } => session,
    };
    if actual != session {
        bail!("foreign session");
    }
    Ok(message)
}

pub fn resource(url: &str) -> Option<(&'static str, &'static [u8])> {
    match url.strip_prefix(ORIGIN)? {
        "/index.html?session=left"
        | "/index.html?session=right"
        | "/index.html?session=recreated" => Some(("text/html", include_bytes!("offscreen.html"))),
        "/offscreen.js" => Some(("text/javascript", include_bytes!("offscreen.js"))),
        "/probe.css" => Some(("text/css", include_bytes!("probe.css"))),
        _ => None,
    }
}

pub fn frame_bytes(width: i32, height: i32) -> Result<usize> {
    if width <= 0 || height <= 0 {
        bail!("invalid frame dimensions");
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("frame size overflow"))?;
    if bytes > MAX_FRAME_BYTES {
        bail!("frame exceeds 16 MiB limit");
    }
    Ok(bytes)
}

pub fn straight_bgra(width: i32, height: i32, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() != frame_bytes(width, height)? {
        bail!("truncated or oversized frame");
    }
    let mut bytes = bytes.to_vec();
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in pixel.iter_mut().take(3) {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    Ok(bytes)
}

pub fn local_point(
    point: (f32, f32),
    origin: (f32, f32),
    bounds: (f32, f32),
    scale: f32,
) -> Result<(i32, i32)> {
    if !scale.is_finite()
        || scale <= 0.0
        || [point.0, point.1, origin.0, origin.1, bounds.0, bounds.1]
            .iter()
            .any(|value| !value.is_finite())
    {
        bail!("invalid pane geometry");
    }
    let point = (point.0 - origin.0, point.1 - origin.1);
    if point.0 < 0.0 || point.1 < 0.0 || point.0 >= bounds.0 || point.1 >= bounds.1 {
        bail!("point outside pane");
    }
    Ok((point.0.floor() as i32, point.1.floor() as i32))
}

#[derive(Default)]
pub struct LatestFrame<T> {
    pub pending: Option<T>,
    pub replaced: u64,
}

impl<T> LatestFrame<T> {
    pub fn publish(&mut self, frame: T) {
        if self.pending.replace(frame).is_some() {
            self.replaced += 1;
        }
    }
    pub fn take(&mut self) -> Option<T> {
        self.pending.take()
    }
}

pub fn validate_runtime(files: &[&str]) -> Result<()> {
    if files
        .iter()
        .any(|file| matches!(*file, "libcef.dll" | "libcef.so"))
        && ["icudtl.dat", "resources.pak", "locales"]
            .iter()
            .all(|required| files.contains(required))
    {
        Ok(())
    } else {
        bail!(
            "CEF runtime missing: rebuild with the pinned offscreen feature and retain copied runtime resources"
        )
    }
}

pub fn enqueue(queue: &mut VecDeque<Message>, value: Message) -> Result<()> {
    if queue.len() >= MAX_MESSAGES {
        bail!("bridge queue full");
    }
    queue.push_back(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_bad_pixels() {
        assert!(frame_bytes(0, 2).is_err());
        assert!(frame_bytes(-1, 2).is_err());
        assert!(frame_bytes(i32::MAX, i32::MAX).is_err());
        assert!(frame_bytes(4096, 4096).is_err());
        assert!(straight_bgra(1, 1, &[1, 2, 3]).is_err());
        assert!(straight_bgra(1, 1, &[0; 8]).is_err());
    }
    #[test]
    fn alpha_and_channel_conversion() -> Result<()> {
        assert_eq!(
            straight_bgra(2, 1, &[25, 50, 100, 128, 1, 2, 3, 255])?,
            [50, 100, 199, 128, 1, 2, 3, 255]
        );
        assert_eq!(straight_bgra(1, 1, &[1, 2, 3, 0])?, [0, 0, 0, 0]);
        Ok(())
    }
    #[test]
    fn points_are_local_dips_not_device_pixels() -> Result<()> {
        assert_eq!(
            local_point((150., 75.), (100., 25.), (200., 100.), 1.75)?,
            (50, 50)
        );
        assert!(local_point((99., 75.), (100., 25.), (200., 100.), 1.75).is_err());
        assert!(local_point((150., 75.), (100., 25.), (200., 100.), f32::NAN).is_err());
        Ok(())
    }
    #[test]
    fn latest_frame_backpressure() {
        let mut latest = LatestFrame::default();
        for frame in 0..1000 {
            latest.publish(frame);
        }
        assert_eq!(latest.take(), Some(999));
        assert_eq!(latest.replaced, 999);
        assert_eq!(latest.take(), None);
    }
    #[test]
    fn session_resource_and_runtime_boundaries() -> Result<()> {
        let hello = r#"{"kind":"Hello","session":"left","request":1}"#;
        assert!(message(&document_url("left"), hello, "left").is_ok());
        assert!(message(&document_url("right"), hello, "left").is_err());
        assert!(message(&document_url("left"), hello, "right").is_err());
        assert!(message(&document_url("left"), &" ".repeat(4097), "left").is_err());
        assert!(resource(&document_url("left")).is_some());
        assert!(resource(&format!("{ORIGIN}/../Cargo.toml")).is_none());
        assert!(resource("https://example.invalid/offscreen.js").is_none());
        assert!(validate_runtime(&["offscreen.exe"]).is_err());
        validate_runtime(&["libcef.dll", "icudtl.dat", "resources.pak", "locales"])?;
        Ok(())
    }
    #[test]
    fn reject_missing_runtime_resources() {
        assert!(validate_runtime(&["libcef.dll"]).is_err());
        assert!(
            validate_runtime(&[
                "libcef-not-a-runtime",
                "icudtl.dat",
                "resources.pak",
                "locales"
            ])
            .is_err()
        );
    }
    #[test]
    fn bridge_queue_is_bounded_without_discarding_accepted_messages() -> Result<()> {
        let mut queue = VecDeque::new();
        for request in 0..MAX_MESSAGES as u32 {
            enqueue(
                &mut queue,
                Message::Hello {
                    session: "left".into(),
                    request,
                },
            )?;
        }
        assert!(
            enqueue(
                &mut queue,
                Message::Hello {
                    session: "left".into(),
                    request: 99
                }
            )
            .is_err()
        );
        assert_eq!(queue.len(), MAX_MESSAGES);
        assert_eq!(
            queue.pop_front(),
            Some(Message::Hello {
                session: "left".into(),
                request: 0
            })
        );
        Ok(())
    }
}
