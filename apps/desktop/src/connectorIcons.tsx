// Icons for connectors. The catalogue names a Lucide icon per entry (as data, in kebab case);
// only the names listed here are bundled, and anything else shows a plug.

import {
  BookOpen,
  Brain,
  Clock,
  Cloud,
  Database,
  FileText,
  FolderOpen,
  GitBranch,
  GitPullRequest,
  Globe,
  HardDrive,
  ListChecks,
  Mail,
  MessageSquare,
  Monitor,
  NotebookText,
  Plug,
  Search,
  Server,
  Terminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";

const ICONS: Record<string, LucideIcon> = {
  "book-open": BookOpen,
  brain: Brain,
  clock: Clock,
  cloud: Cloud,
  database: Database,
  "file-text": FileText,
  "folder-open": FolderOpen,
  "git-branch": GitBranch,
  "git-pull-request": GitPullRequest,
  globe: Globe,
  "hard-drive": HardDrive,
  "list-checks": ListChecks,
  mail: Mail,
  "message-square": MessageSquare,
  monitor: Monitor,
  "notebook-text": NotebookText,
  plug: Plug,
  search: Search,
  server: Server,
  terminal: Terminal,
  wrench: Wrench,
};

/** A connector's icon on a raised tile. Decorative; the card supplies the name. */
export function ConnectorIcon({ icon, size = "normal" }: { icon?: string | null; size?: "normal" | "large" }) {
  const Icon = (icon && ICONS[icon]) || Plug;
  return (
    <span className={`connector-icon ${size}`} aria-hidden="true">
      <Icon />
    </span>
  );
}
