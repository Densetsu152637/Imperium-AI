// The Installed view: plugins already on this computer, read-only for now (turning them on or off,
// updating and removing arrive with the install flow).

import { Puzzle } from "lucide-react";
import type { PluginsListResult } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { matches, plain } from "./model";

const part = (n: number, noun: string) => (n > 0 ? `${n} ${noun}${n === 1 ? "" : "s"}` : null);

export function InstalledView({ result, query, onBrowse }: { result: PluginsListResult | null; query: string; onBrowse: () => void }) {
  if (!result) return <LoadingState />;
  const shown = result.plugins.filter((p) => matches(query, p.name, p.id, p.origin, p.source));
  return (
    <>
      {result.problems.map((problem) => (
        <Notice key={problem} tone="warn">
          {plain(problem)}
        </Notice>
      ))}
      {result.plugins.length === 0 ? (
        <EmptyState
          icon={Puzzle}
          title="No plugins installed"
          body="Plugins add skills, commands, hooks and servers."
          action={
            <button className="primary" onClick={onBrowse}>
              Browse plugins
            </button>
          }
        />
      ) : shown.length === 0 ? (
        <EmptyState title={`No installed plugins match “${query}”`} />
      ) : (
        <ul className="connector-rows">
          {shown.map((plugin) => (
            <li key={plugin.folder} className="connector-row" data-enabled={plugin.enabled && !plugin.withheld}>
              <span className="connector-icon small" aria-hidden="true">
                <Puzzle />
              </span>
              <div className="connector-row-title">
                <strong>{plain(plugin.name)}</strong>
                {plugin.withheld && <Badge tone="warn">Withheld</Badge>}
                {!plugin.enabled && <Badge>Off</Badge>}
                {plugin.missing_secrets.length > 0 && <Badge tone="warn">Needs secrets</Badge>}
                <span className="connector-row-counts">
                  {[part(plugin.hooks, "hook"), plugin.servers.length > 0 ? `servers: ${plugin.servers.map(plain).join(", ")}` : null].filter(Boolean).join(" · ")}
                </span>
              </div>
              <div className="connector-row-detail">
                <span className="connector-row-source">
                  {[plugin.id && plain(plugin.id), plugin.origin && `from ${plain(plugin.origin)}`, plugin.commit && `commit ${plain(plugin.commit).slice(0, 8)}`]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
                {plugin.reason && <span className="connector-row-source">{plain(plugin.reason)}</span>}
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
