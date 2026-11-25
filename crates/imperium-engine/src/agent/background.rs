use super::*;

impl Agent {
    /// Tells the agent about the background commands it started that ended on their own since
    /// its last model call, one user message each (in the order they were started), wrapped in
    /// `<background-notice>` so it reads them as system reports, not as the user's words. The
    /// root is also told of commands no agent owns. Each command is reported once; a command the
    /// agent stopped, or whose end it already read with `command_output`, is not. Called at the
    /// steering safe point, so tool calls and their results stay together; a command that ends
    /// while the agent is idle is reported at the first safe point of its next turn (nothing
    /// wakes an idle session for it). Each notice is also shown to clients as a `notice` event.
    pub(super) fn take_background_notices(&mut self) {
        let owner = self.workspace.owner().map(str::to_string);
        let finished = self
            .workspace
            .processes()
            .take_finished(owner.as_deref(), self.can_delegate);
        for finished in finished {
            self.emit(SessionEvent::Notice {
                message: finished.summary(),
            });
            let (content, spans) = join_parts(vec![(Category::User, finished.framed())]);
            self.spans.insert(self.messages.len(), spans);
            self.messages.push(ChatMessage::User {
                content,
                images: Vec::new(),
            });
        }
    }
}
