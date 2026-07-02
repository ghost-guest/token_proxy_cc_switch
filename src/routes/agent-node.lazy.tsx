import { createLazyFileRoute } from "@tanstack/react-router";

import { AgentNodePage } from "@/features/agent-node/pages/agent-node-page";

export const Route = createLazyFileRoute("/agent-node")({
  component: AgentNodeRoute,
});

function AgentNodeRoute() {
  return <AgentNodePage />;
}
