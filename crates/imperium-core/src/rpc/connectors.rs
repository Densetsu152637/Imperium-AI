use super::*;

impl Connection {
    /// The `connectors` methods: `None` for a method of another group.
    pub(super) fn dispatch_connectors(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            McpList::NAME => call::<McpList>(c.params.take(), |_: McpListParams| {
                Ok(crate::connectors::list(&self.state))
            }),
            McpCatalog::NAME => call::<McpCatalog>(c.params.take(), |_: McpCatalogParams| {
                Ok(crate::connectors::catalog(&self.state))
            }),
            McpSave::NAME => call::<McpSave>(c.params.take(), |params| {
                crate::connectors::save(&self.state, params)
            }),
            McpInstall::NAME => call::<McpInstall>(c.params.take(), |params| {
                crate::connectors::install(&self.state, params)
            }),
            McpSetEnabled::NAME => call::<McpSetEnabled>(c.params.take(), |params| {
                crate::connectors::set_enabled(&self.state, params)
            }),
            McpRemove::NAME => call::<McpRemove>(c.params.take(), |params| {
                crate::connectors::remove(&self.state, params)
            }),
            McpTest::NAME => call_later::<McpTest, _>(c.params.take(), c.later, |params| {
                crate::connectors::test(Arc::clone(&self.state), params)
            }),
            McpLogin::NAME => call_later::<McpLogin, _>(c.params.take(), c.later, |params| {
                crate::connectors::login(Arc::clone(&self.state), params)
            }),
            McpLogout::NAME => call_later::<McpLogout, _>(c.params.take(), c.later, |params| {
                crate::connectors::logout(Arc::clone(&self.state), params)
            }),
            _ => return None,
        })
    }
}
