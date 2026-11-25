use super::*;

impl Agent {
    /// Tells the agent about the background commands that ended on their own since its last
    /// model call, one user message each (in the order they were started), wrapped in
    /// `<background-notice>` so it reads them as system notices, not as the user's words. Each
    /// command is reported once; a command the agent stopped, or whose end it already read
    /// with `command_output`, is not. Called at the steering safe point, so tool calls and
    /// their results stay together; a command that ends while the agent is idle is reported
    /// at the first safe point of its next turn (nothing wakes an idle session for it).
    pub(super) fn take_background_notices(&mut self) {
        // Subagents may share the root's workspace and with it the table: only the root is told.
        if !self.can_delegate {
            return;
        }
        for finished in self.workspace.processes().take_finished() {
            let text = format!(
                "<background-notice>\n{}\n</background-notice>",
                finished.notice()
            );
            let (content, spans) = join_parts(vec![(Category::User, text)]);
            self.spans.insert(self.messages.len(), spans);
            self.messages.push(ChatMessage::User {
                content,
                images: Vec::new(),
            });
        }
    }
}
