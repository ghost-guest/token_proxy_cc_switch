import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { UpstreamsCard } from "@/features/config/cards/upstreams-card";
import { createEmptyUpstream, createModelMapping } from "@/features/config/form";
import type { ConfigForm, UpstreamForm } from "@/features/config/types";

const STRATEGY: ConfigForm["upstreamStrategy"] = {
  order: "fill_first",
  dispatchType: "serial",
  hedgeDelayMs: "250",
  maxParallel: "2",
};

afterEach(() => {
  cleanup();
});

function buildUpstream(id: string, enabled: boolean, models: string[]): UpstreamForm {
  return {
    ...createEmptyUpstream(),
    id,
    providers: ["openai-response"],
    baseUrl: `https://${id}.example.com/v1`,
    enabled,
    modelMappings: models.map((model) => createModelMapping(model, model)),
  };
}

function renderCard(upstreams: UpstreamForm[], onChange = vi.fn()) {
  render(
    <UpstreamsCard
      upstreams={upstreams}
      appProxyUrl=""
      strategy={STRATEGY}
      showApiKeys={false}
      providerOptions={["openai-response"]}
      onToggleApiKeys={vi.fn()}
      onStrategyChange={vi.fn()}
      onAdd={vi.fn()}
      onRemove={vi.fn()}
      onChange={onChange}
    />
  );
  return { onChange };
}

describe("config/cards/UpstreamsCard", () => {
  it("shows model mappings next to each upstream", () => {
    renderCard([
      buildUpstream("deepseek", true, ["deepseek-v4-flash"]),
      buildUpstream("tokenskingdom", false, ["gpt-5.5"]),
    ]);

    expect(screen.getByText("Models")).toBeInTheDocument();
    expect(screen.getByText("deepseek-v4-flash")).toBeInTheDocument();
    expect(screen.getByText("gpt-5.5")).toBeInTheDocument();
  });

  it("switches priority without disabling other upstreams", async () => {
    const user = userEvent.setup();
    const upstreams = [
      { ...buildUpstream("deepseek", true, ["deepseek-v4-flash"]), priority: "10" },
      { ...buildUpstream("tokenskingdom", false, ["gpt-5.5"]), priority: "20" },
    ];
    const { onChange } = renderCard(upstreams);

    await user.click(screen.getByRole("button", { name: "Switch to Upstream 1" }));

    expect(onChange).toHaveBeenCalledWith(1, { enabled: true, priority: "21" });
    expect(onChange).not.toHaveBeenCalledWith(0, expect.objectContaining({ enabled: false }));

    onChange.mockClear();
    await user.click(screen.getByRole("button", { name: "Active Upstream 2" }));

    expect(onChange).toHaveBeenCalledWith(0, { enabled: true, priority: "21" });
  });
});
