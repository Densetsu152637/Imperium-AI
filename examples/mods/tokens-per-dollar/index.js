// An example UI mod: totals tokens and cost across model calls and shows tokens per dollar.
// Install by copying this folder to <config>/mods/ and reconnecting the desktop app.
import { defineMod } from "@imperium/mod-sdk";

export default defineMod({
  id: "tokens-per-dollar",
  permissions: ["telemetry:read"],
  contributes: { panels: [{ id: "tpd", title: "Tokens / $", slot: "sidebar.right" }] },
  activate(ctx) {
    let tokens = 0;
    let cost = 0;
    let calls = 0;
    const render = () => {
      const perDollar = cost > 0 ? Math.round(tokens / cost).toLocaleString() : "n/a";
      ctx.panel("tpd").render(
        `<p><strong>${perDollar}</strong> tokens per dollar</p>` +
          `<p style="color: var(--muted)">${tokens.toLocaleString()} tokens, $${cost.toFixed(4)}, ${calls} calls</p>`,
      );
    };
    ctx.telemetry.subscribe("call.completed", (call) => {
      tokens += call.promptTokens + call.completionTokens;
      cost += call.costUsd ?? 0;
      calls += 1;
      render();
    });
    render();
  },
});
