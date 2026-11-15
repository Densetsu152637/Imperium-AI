use super::*;

impl Connection {
    /// The `policy` methods: `None` for a method of another group.
    pub(super) fn dispatch_policy(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            BudgetsGet::NAME => call::<BudgetsGet>(c.params.take(), |_: BudgetsGetParams| {
                crate::budgets::get(&self.state)
            }),
            BudgetsSet::NAME => call::<BudgetsSet>(c.params.take(), |params| {
                crate::budgets::set(&self.state, params)
            }),
            BudgetsUsage::NAME => call::<BudgetsUsage>(c.params.take(), |params| {
                crate::budgets::usage(&self.state, params)
            }),
            PermissionsGet::NAME => {
                call::<PermissionsGet>(c.params.take(), |_: PermissionsGetParams| {
                    crate::permissions::get(&self.state)
                })
            }
            PermissionsSet::NAME => call::<PermissionsSet>(c.params.take(), |params| {
                crate::permissions::set(&self.state, params)
            }),
            PermissionsAddRule::NAME => call::<PermissionsAddRule>(c.params.take(), |params| {
                crate::permissions::add_rule(&self.state, params)
            }),
            InitiativeGet::NAME => call::<InitiativeGet>(c.params.take(), |params| {
                crate::initiative::get(&self.state, params)
            }),
            InitiativeStart::NAME => call::<InitiativeStart>(c.params.take(), |params| {
                crate::initiative::start_suggestion(&self.state, params)
            }),
            InitiativeDismiss::NAME => call::<InitiativeDismiss>(c.params.take(), |params| {
                crate::initiative::dismiss(&self.state, params).map(|()| Accepted {})
            }),
            InitiativeReview::NAME => call::<InitiativeReview>(c.params.take(), |params| {
                crate::initiative::review_now(&self.state, params).map(|()| Accepted {})
            }),
            ProjectList::NAME => call::<ProjectList>(c.params.take(), |_: ProjectListParams| {
                Ok(ProjectListResult {
                    agents: self
                        .state
                        .project_agents()
                        .list()
                        .into_iter()
                        .map(|(project, session_id)| ProjectAgentRow {
                            project: project.display().to_string(),
                            session_id,
                        })
                        .collect(),
                })
            }),
            ProjectMessage::NAME => {
                call::<ProjectMessage>(c.params.take(), |params: ProjectMessageParams| {
                    Ok(ProjectMessageResult {
                        delivered: self
                            .state
                            .tell_project_agent(std::path::Path::new(&params.project), params.text),
                    })
                })
            }
            ApprovalList::NAME => call::<ApprovalList>(c.params.take(), |_: ApprovalListParams| {
                Ok(ApprovalListResult {
                    approvals: self.state.approvals().list(),
                })
            }),
            ApprovalRespond::NAME => {
                call::<ApprovalRespond>(c.params.take(), |params: ApprovalRespondParams| {
                    Ok(ApprovalRespondResult {
                        found: self.state.approvals().respond(params.id, params.approve),
                    })
                })
            }
            ScheduleList::NAME => call::<ScheduleList>(c.params.take(), |_: ScheduleListParams| {
                Ok(self
                    .state
                    .scheduler()
                    .map(|scheduler| scheduler.list())
                    .unwrap_or(ScheduleListResult {
                        schedules: Vec::new(),
                    }))
            }),
            ScheduleRun::NAME => {
                call::<ScheduleRun>(c.params.take(), |params: ScheduleRunParams| {
                    let scheduler = self.state.scheduler().ok_or_else(|| {
                        ErrorObject::new(codes::INTERNAL_ERROR, "the scheduler is not running")
                    })?;
                    let started = scheduler
                        .run_now(&self.state, &params.id)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(ScheduleRunResult { started })
                })
            }
            _ => return None,
        })
    }
}
