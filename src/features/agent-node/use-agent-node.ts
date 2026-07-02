import { invoke } from "@tauri-apps/api/core";
import { useCallback, useState } from "react";

import type {
  AgentNodeConfig,
  AgentNodeRequestState,
  AgentNodeServiceStatus,
} from "@/features/config/types";
import { parseError } from "@/lib/error";

const EMPTY_AGENT_NODE_CONFIG: AgentNodeConfig = {
  enabled: false,
  server_url: "",
  api_key: "",
  hostname: null,
};

export function useAgentNode() {
  const [config, setConfig] = useState<AgentNodeConfig>(EMPTY_AGENT_NODE_CONFIG);
  const [status, setStatus] = useState<AgentNodeServiceStatus | null>(null);
  const [requestState, setRequestState] = useState<AgentNodeRequestState>("idle");
  const [message, setMessage] = useState("");
  const [apiKeyVisible, setApiKeyVisible] = useState(false);

  const updateConfig = useCallback((patch: Partial<AgentNodeConfig>) => {
    setConfig((prev) => ({ ...prev, ...patch }));
  }, []);

  const runStatusCommand = useCallback(
    async (
      command:
        | "agent_node_status"
        | "agent_node_start"
        | "agent_node_stop"
        | "agent_node_restart"
    ) => {
      setRequestState("working");
      setMessage("");
      try {
        const nextStatus = await invoke<AgentNodeServiceStatus>(command);
        setStatus(nextStatus);
        setRequestState("idle");
      } catch (error) {
        setRequestState("error");
        setMessage(parseError(error));
      }
    },
    []
  );

  const load = useCallback(async () => {
    setRequestState("working");
    setMessage("");
    try {
      const nextConfig = await invoke<AgentNodeConfig>("agent_node_read_config");
      const nextStatus = await invoke<AgentNodeServiceStatus>("agent_node_status");
      setConfig(nextConfig);
      setStatus(nextStatus);
      setRequestState("idle");
    } catch (error) {
      setRequestState("error");
      setMessage(parseError(error));
    }
  }, []);

  const save = useCallback(async () => {
    setRequestState("working");
    setMessage("");
    try {
      const nextStatus = await invoke<AgentNodeServiceStatus>("agent_node_save_config", {
        config,
      });
      setStatus(nextStatus);
      setRequestState("idle");
    } catch (error) {
      setRequestState("error");
      setMessage(parseError(error));
    }
  }, [config]);

  return {
    config,
    status,
    requestState,
    message,
    apiKeyVisible,
    updateConfig,
    toggleApiKey: () => setApiKeyVisible((value) => !value),
    load,
    refresh: () => runStatusCommand("agent_node_status"),
    save,
    start: () => runStatusCommand("agent_node_start"),
    stop: () => runStatusCommand("agent_node_stop"),
    restart: () => runStatusCommand("agent_node_restart"),
  };
}
