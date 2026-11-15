//! Transient messages: errors and confirmations appear in the corner of the
//! window and fade on their own, so nothing ever blocks the user.

use super::theme;
use eframe::egui::{self, Align2, Color32};
use std::time::{Duration, Instant};

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

    /// Paints the messages, newest at the bottom, above `bottom_right`.
    /// Click a message to dismiss it.
    pub fn show(&mut self, ctx: &egui::Context, bottom_right: egui::Pos2) {
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
            let accent = match toast.kind {
                ToastKind::Info => theme::ACCENT,
                ToastKind::Success => theme::PLAY,
                ToastKind::Error => theme::ERROR,
            };
            let area = egui::Area::new(egui::Id::unique(("toast", index)))
                .order(egui::Order::Foreground)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(egui::pos2(bottom_right.x, bottom))
                .show(ctx, |ui| {
                    ui.set_opacity(opacity);
                    let frame = egui::Frame::NONE
                        .fill(theme::BG_RAISED)
                        .stroke(theme::hairline(theme::BORDER_STRONG))
                        .corner_radius(theme::RADIUS)
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha(90),
                        })
                        .inner_margin(egui::Margin { left: 14, right: 12, top: 9, bottom: 9 });
                    let response = frame
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.add(egui::Label::new(egui::RichText::new(&toast.text).color(theme::TEXT)).wrap());
                        })
                        .response;
                    // A colored edge says what kind of message this is.
                    let edge = egui::Rect::from_min_max(
                        response.rect.left_top() + egui::vec2(0.0, 5.0),
                        response.rect.left_bottom() + egui::vec2(3.0, -5.0),
                    );
                    ui.painter().rect_filled(edge, 2.0, accent);
                    response.interact(egui::Sense::click())
                });
            if area.inner.clicked() {
                dismissed = Some(index);
            }
            bottom = area.response.rect.top() - 8.0;
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
