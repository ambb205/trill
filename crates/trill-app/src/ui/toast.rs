//! Transient messages: errors and confirmations appear in the corner of the
//! window and fade on their own, so nothing ever blocks the user.

use eframe::egui::{self, Align2};
use std::time::{Duration, Instant};
use trill_noir::{Theme, paint};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

struct Toast {
    kind: ToastKind,
    text: String,
    created: Instant,
    lifetime: Duration,
}

const FADE: Duration = Duration::from_millis(400);
/// More than this many at once would cover the window; the oldest go first.
const MAX_VISIBLE: usize = 5;

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Info, text.into(), Duration::from_secs(4));
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Success, text.into(), Duration::from_secs(5));
    }

    /// Errors stay longer: the user did not expect them and has to read them.
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Error, text.into(), Duration::from_secs(8));
    }

    /// A message with a custom lifetime, for results worth studying.
    pub fn lasting(&mut self, kind: ToastKind, text: impl Into<String>, lifetime: Duration) {
        self.push(kind, text.into(), lifetime);
    }

    fn push(&mut self, kind: ToastKind, text: String, lifetime: Duration) {
        // A drag that keeps failing would otherwise stack the same message
        // once per frame.
        if let Some(existing) = self.items.iter_mut().find(|toast| toast.kind == kind && toast.text == text) {
            existing.created = Instant::now();
            return;
        }
        self.items.push(Toast { kind, text, created: Instant::now(), lifetime });
        if self.items.len() > MAX_VISIBLE {
            self.items.remove(0);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// What the messages on show say, oldest first.
    #[cfg(test)]
    pub fn texts(&self) -> Vec<&str> {
        self.items.iter().map(|toast| toast.text.as_str()).collect()
    }

    /// Paints the messages, newest at the bottom, above `bottom_right`.
    /// Click a message to dismiss it.
    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme, bottom_right: egui::Pos2) {
        let now = Instant::now();
        self.items.retain(|toast| now.duration_since(toast.created) < toast.lifetime);
        if self.items.is_empty() {
            return;
        }

        let mut dismissed = None;
        let mut bottom = bottom_right.y;
        for (index, toast) in self.items.iter().enumerate().rev() {
            let remaining = toast.lifetime.saturating_sub(now.duration_since(toast.created));
            let opacity = (remaining.as_secs_f32() / FADE.as_secs_f32()).min(1.0);
            // A status dot says what kind of message this is.
            let dot = match toast.kind {
                ToastKind::Info => theme.info,
                ToastKind::Success => theme.ok,
                ToastKind::Error => theme.signal,
            };
            let area = egui::Area::new(egui::Id::unique(("toast", index)))
                .order(egui::Order::Foreground)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(egui::pos2(bottom_right.x, bottom))
                .show(ctx, |ui| {
                    ui.set_opacity(opacity);
                    let frame = egui::Frame::NONE
                        .fill(theme.raised)
                        .stroke(egui::Stroke::new(1.0, theme.hairline))
                        .inner_margin(egui::Margin { left: 23, right: 12, top: 8, bottom: 8 });
                    let response = frame
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.add(egui::Label::new(egui::RichText::new(&toast.text).color(theme.ink)).wrap());
                        })
                        .response;
                    // Level with the first line of the text.
                    let first_line = response.rect.top() + 8.0 + theme.text.body.size * 0.62;
                    paint::dot(
                        ui.painter(),
                        egui::pos2(response.rect.left() + 10.0, first_line),
                        theme.metric.dot,
                        dot,
                    );
                    response.interact(egui::Sense::click())
                });
            if area.inner.clicked() {
                dismissed = Some(index);
            }
            bottom = area.response.rect.top() - 6.0;
        }
        if let Some(index) = dismissed {
            self.items.remove(index);
        }
        // Keep animating the fade and the expiry even if nothing else moves.
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_messages_do_not_stack() {
        let mut toasts = Toasts::default();
        toasts.error("track 9 not found");
        toasts.error("track 9 not found");
        toasts.info("track 9 not found");
        assert_eq!(toasts.len(), 2, "same text, different kind is a different message");
    }

    #[test]
    fn the_oldest_messages_make_room() {
        let mut toasts = Toasts::default();
        for index in 0..MAX_VISIBLE + 3 {
            toasts.info(format!("message {index}"));
        }
        assert_eq!(toasts.len(), MAX_VISIBLE);
        assert_eq!(toasts.items[0].text, "message 3");
    }
}
