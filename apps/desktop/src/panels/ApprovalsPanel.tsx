// Requests from unattended work (schedules, auto-fix) waiting for the user's yes or no.

import { Check, X } from "lucide-react";

import type { ApprovalInfo } from "@imperium/protocol";

import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";

export function ApprovalsPanel({
  approvals,
  onRespond,
}: {
  approvals: ApprovalInfo[];
  onRespond: (id: number, approve: boolean) => void;
}) {
  if (approvals.length === 0) {
    return <EmptyState className="compact" title="Nothing to approve" body="Requests from schedules and auto-fix wait here for your yes or no." />;
  }
  return (
    <ul className="approvals">
      {approvals.map((approval) => (
        <li key={approval.id} className="approval">
          <p>
            <Badge>{approval.kind}</Badge> {approval.summary}
          </p>
          <div className="approval-actions">
            <button className="primary small" onClick={() => onRespond(approval.id, true)}>
              <Check aria-hidden="true" /> Approve
            </button>
            <button className="ghost small" onClick={() => onRespond(approval.id, false)}>
              <X aria-hidden="true" /> Decline
            </button>
          </div>
        </li>
      ))}
    </ul>
  );
}
