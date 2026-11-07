// Requests from unattended work (schedules, auto-fix) waiting for the user's yes or no.

import type { ApprovalInfo } from "@imperium/protocol";

export function ApprovalsPanel({
  approvals,
  onRespond,
}: {
  approvals: ApprovalInfo[];
  onRespond: (id: number, approve: boolean) => void;
}) {
  if (approvals.length === 0) {
    return <p className="empty">Nothing is waiting for approval.</p>;
  }
  return (
    <ul className="approvals">
      {approvals.map((approval) => (
        <li key={approval.id} className="approval">
          <p>
            <span className="badge">{approval.kind}</span> {approval.summary}
          </p>
          <div className="approval-actions">
            <button className="primary" onClick={() => onRespond(approval.id, true)}>
              Approve
            </button>
            <button onClick={() => onRespond(approval.id, false)}>Decline</button>
          </div>
        </li>
      ))}
    </ul>
  );
}
