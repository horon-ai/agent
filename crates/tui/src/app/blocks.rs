//! Taking a popup row into the draft, and attaching images to it.

use super::App;

impl App {
    /// Take the command the popup has selected.
    ///
    /// The whole line becomes the command, so nothing of the old prefix
    /// survives to be sent as an argument the user did not type. A command is
    /// followed by a space, which closes the popup and leaves the caret where
    /// its arguments go; a skill becomes a block before the draft instead, so
    /// the line is free for the request.
    pub(crate) fn complete_selected(&mut self) -> bool {
        let Some(name) = self.slash.selected_name().map(str::to_string) else {
            return false;
        };
        self.history.reset();
        self.path_cycle = None;
        match name.strip_prefix("skill:") {
            Some(skill) => {
                self.attachments.set_skill(skill);
                self.input.clear();
            }
            None => self.input.set_text(&format!("/{name} ")),
        }
        self.sync_slash();
        true
    }

    /// Attach an image to the draft, or say why it could not be.
    pub(crate) fn attach_image(&mut self, image: Result<crate::attachments::Image, crate::attachments::ImageError>) {
        match image {
            Ok(image) => {
                self.attachments.add_image(image);
            }
            Err(message) => self.warn(format!("image: {message}")),
        }
    }
}
