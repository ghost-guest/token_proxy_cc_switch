import { useEffect } from "react";

import { AppShell } from "@/layouts/app-shell";
import { AgentNodeCard } from "@/features/config/cards";
import { useAgentNode } from "@/features/agent-node/use-agent-node";
import { m } from "@/paraglide/messages.js";

export function AgentNodePage() {
  const agentNode = useAgentNode();
  const { load } = agentNode;

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <AppShell title={m.agent_node_title()}>
      <div className="flex flex-col gap-4 px-4 lg:px-6">
        <AgentNodeCard
          config={agentNode.config}
          status={agentNode.status}
          requestState={agentNode.requestState}
          message={agentNode.message}
          apiKeyVisible={agentNode.apiKeyVisible}
          onConfigChange={agentNode.updateConfig}
          onToggleApiKey={agentNode.toggleApiKey}
          onRefresh={agentNode.refresh}
          onSave={agentNode.save}
          onStart={agentNode.start}
          onStop={agentNode.stop}
          onRestart={agentNode.restart}
        />
      </div>
    </AppShell>
  );
}
