// A model named in the interface: its alias when it has one, with the real id or spec on hover
// (modelNames.ts).

import { modelName, modelTitle, useModelNames } from "../modelNames";

export function ModelName({ name, className }: { name: string; className?: string }) {
  const aliases = useModelNames();
  return (
    <span className={className} title={modelTitle(aliases, name)}>
      {modelName(aliases, name)}
    </span>
  );
}
