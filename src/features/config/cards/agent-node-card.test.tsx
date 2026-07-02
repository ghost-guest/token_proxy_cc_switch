import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AgentNodeCard } from "@/features/config/cards/agent-node-card";
import type { AgentNodeConfig } from "@/features/config/types";
import { m } from "@/paraglide/messages.js";
import { setLocale } from "@/paraglide/runtime.js";

const AGENT_NODE_CONFIG: AgentNodeConfig = {
  enabled: true,
  server_url: "https://agent.example.com",
  api_key: "acn_test",
  hostname: "desk-1",
};

describe("config/cards/AgentNodeCard", () => {
  afterEach(() => {
    cleanup();
    setLocale("en", { reload: false });
  });

  it("uses localized labels for the Agent Node controls", () => {
    setLocale("zh", { reload: false });

    render(
      <AgentNodeCard
        config={AGENT_NODE_CONFIG}
        status={{
          state: "running",
          enabled: true,
          server_url: AGENT_NODE_CONFIG.server_url,
          hostname: AGENT_NODE_CONFIG.hostname,
          last_error: null,
          started_at_ms: 1,
        }}
        requestState="idle"
        message=""
        apiKeyVisible={false}
        onConfigChange={vi.fn()}
        onToggleApiKey={vi.fn()}
        onRefresh={vi.fn()}
        onSave={vi.fn()}
        onStart={vi.fn()}
        onStop={vi.fn()}
        onRestart={vi.fn()}
      />
    );

    expect(screen.getByText(m.agent_node_title({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByText(m.agent_node_desc({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByText(m.proxy_service_state_label({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByText(m.proxy_service_badge_running({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByLabelText(m.agent_node_server_url_label({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByLabelText(m.agent_node_hostname_label({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByText(m.agent_node_hostname_help({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByLabelText(m.agent_node_api_key_label({}, { locale: "zh" }))).toBeInTheDocument();
    expect(screen.getByRole("button", { name: m.common_save({}, { locale: "zh" }) })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: m.proxy_service_stop({}, { locale: "zh" }) })).toBeInTheDocument();
    expect(screen.queryByText("Connect this desktop app to a public Agent Console.")).not.toBeInTheDocument();
  });
});
