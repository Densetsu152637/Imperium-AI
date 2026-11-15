use super::*;

impl Connection {
    /// The `access` methods: `None` for a method of another group.
    pub(super) fn dispatch_access(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            PhonePair::NAME => call::<PhonePair>(c.params.take(), |_: PhonePairParams| {
                Ok(PhonePairResult {
                    pairing: self.state.devices().new_code(),
                    url: self.state.phone_url(),
                })
            }),
            PhoneDevices::NAME => call::<PhoneDevices>(c.params.take(), |_: PhoneDevicesParams| {
                Ok(PhoneDevicesResult {
                    devices: self.state.devices().list(),
                })
            }),
            PhoneRevoke::NAME => {
                call::<PhoneRevoke>(c.params.take(), |params: PhoneRevokeParams| {
                    let found = self
                        .state
                        .devices()
                        .revoke(&params.id)
                        .map_err(|message| ErrorObject::new(codes::INTERNAL_ERROR, message))?;
                    Ok(PhoneRevokeResult { found })
                })
            }
            ApiGet::NAME => call::<ApiGet>(c.params.take(), |_: ApiGetParams| {
                Ok(crate::openai::status(&self.state))
            }),
            ApiSet::NAME => call_later::<ApiSet, _>(c.params.take(), c.later, |params| {
                crate::openai::set(Arc::clone(&self.state), params)
            }),
            ApiToken::NAME => call::<ApiToken>(c.params.take(), |params| {
                crate::openai::token(&self.state, params)
            }),
            _ => return None,
        })
    }
}
