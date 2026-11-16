use super::*;

impl Connection {
    /// The `system` methods: `None` for a method of another group.
    pub(super) fn dispatch_system(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            EventsSubscribe::NAME => {
                call::<EventsSubscribe>(c.params.take(), |params: EventsSubscribeParams| {
                    self.events.subscribe(&params.topics)?;
                    Ok(self.events.selection())
                })
            }
            EventsUnsubscribe::NAME => {
                call::<EventsUnsubscribe>(c.params.take(), |params: EventsUnsubscribeParams| {
                    self.events.unsubscribe(&params.topics)?;
                    Ok(self.events.selection())
                })
            }
            SystemResources::NAME => call_later::<SystemResources, _>(
                c.params.take(),
                c.later,
                |_: SystemResourcesParams| {
                    let state = Arc::clone(&self.state);
                    async move { Ok(crate::resources::snapshot(&state).await) }
                },
            ),
            SystemResourcesSubscribe::NAME => call::<SystemResourcesSubscribe>(
                c.params.take(),
                |params: SystemResourcesSubscribeParams| {
                    let monitor = Arc::clone(self.state.resource_monitor());
                    if !params.on {
                        self.resources = None;
                    } else if self.resources.is_none() {
                        self.resources = Some(monitor.watch(&self.state));
                    }
                    Ok(SystemResourcesSubscribeResult {
                        on: params.on,
                        interval_ms: u32::try_from(monitor.interval().as_millis())
                            .unwrap_or(u32::MAX),
                    })
                },
            ),
            Shutdown::NAME => call::<Shutdown>(c.params.take(), |_: ShutdownParams| {
                tracing::info!("a client asked imperium-core to stop");
                self.state.request_shutdown();
                Ok(ShutdownResult {
                    pid: std::process::id(),
                })
            }),
            SystemLease::NAME => {
                call::<SystemLease>(c.params.take(), |params: SystemLeaseParams| {
                    // One lease per connection: asking again returns the one held.
                    if let Some(held) = &self.lease {
                        return Ok(SystemLeaseResult { id: held.id() });
                    }
                    let held = self
                        .state
                        .acquire_lease(params.kind, params.pid, &params.label)
                        .map_err(|_| {
                            ErrorObject::new(
                                codes::DAEMON_STOPPING,
                                "imperium-core is exiting because nothing uses it; start a new one",
                            )
                        })?;
                    let id = held.id();
                    self.lease = Some(held);
                    Ok(SystemLeaseResult { id })
                })
            }
            SystemLifetime::NAME => {
                call::<SystemLifetime>(c.params.take(), |params: SystemLifetimeParams| {
                    if let Some(persistent) = params.persistent {
                        self.state.set_persistent(persistent);
                    }
                    Ok(self.state.lifetime_status())
                })
            }
            Ping::NAME => call::<Ping>(c.params.take(), |_: PingParams| {
                Ok(PingResult {
                    uptime_ms: self.state.uptime_ms(),
                })
            }),
            _ => return None,
        })
    }
}
