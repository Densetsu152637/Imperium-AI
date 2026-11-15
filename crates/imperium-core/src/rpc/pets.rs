use super::*;

impl Connection {
    /// The `pets` methods: `None` for a method of another group.
    pub(super) fn dispatch_pets(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            PetList::NAME => call::<PetList>(c.params.take(), |_: PetListParams| {
                Ok(match self.state.pets() {
                    Some(pets) => PetListResult {
                        pets: pets.pets(),
                        selected: Some(pets.selected().to_string()),
                        state: pets.state(),
                    },
                    // Pets are off (or none loaded): still list what is installed.
                    None => PetListResult {
                        pets: crate::pets::discover_installed(&self.state)
                            .0
                            .iter()
                            .map(crate::pets::pet_info)
                            .collect(),
                        selected: None,
                        state: None,
                    },
                })
            }),
            PetStrip::NAME => call::<PetStrip>(c.params.take(), |params: PetStripParams| {
                let png = match self.state.pets() {
                    Some(pets) => pets.strip(params.pet_id.as_deref(), &params.animation),
                    // Without the engine there is no selected pet, so one must be named.
                    None => params.pet_id.as_deref().and_then(|id| {
                        crate::pets::strip_of(
                            &crate::pets::discover_installed(&self.state).0,
                            id,
                            &params.animation,
                        )
                    }),
                }
                .ok_or_else(|| {
                    ErrorObject::new(
                        codes::INVALID_PARAMS,
                        format!("the pet does not draw {}", params.animation),
                    )
                })?;
                Ok(PetStripResult {
                    png_base64: base64::engine::general_purpose::STANDARD.encode(png),
                })
            }),
            PetInteract::NAME => {
                call::<PetInteract>(c.params.take(), |params: PetInteractParams| {
                    if let Some(pets) = self.state.pets() {
                        let animation = match params.kind.as_str() {
                            "hide" => "duck-in",
                            "show" => "pop-out",
                            _ => "petted",
                        };
                        pets.report(
                            "user".into(),
                            imperium_pets::PetEvent::Once(animation.into()),
                        );
                    }
                    Ok(PetInteractResult {})
                })
            }
            _ => return None,
        })
    }
}
