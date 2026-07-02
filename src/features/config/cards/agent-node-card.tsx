import { Loader2, Play, RefreshCw, RotateCcw, Square } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { PasswordInput } from "@/components/ui/password-input";
import { Switch } from "@/components/ui/switch";
import type {
  AgentNodeConfig,
  AgentNodeRequestState,
  AgentNodeServiceStatus,
} from "@/features/config/types";
import { cn } from "@/lib/utils";
import { m } from "@/paraglide/messages.js";

type AgentNodeCardProps = {
  config: AgentNodeConfig;
  status: AgentNodeServiceStatus | null;
  requestState: AgentNodeRequestState;
  message: string;
  apiKeyVisible: boolean;
  onConfigChange: (patch: Partial<AgentNodeConfig>) => void;
  onToggleApiKey: () => void;
  onRefresh: () => void;
  onSave: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
};

function resolveBadge(status: AgentNodeServiceStatus | null, message: string) {
  if (message || status?.last_error) {
    return { label: m.proxy_service_badge_error(), variant: "destructive" as const };
  }
  if (!status) {
    return { label: m.proxy_service_badge_unknown(), variant: "outline" as const };
  }
  if (status.state === "running") {
    return { label: m.proxy_service_badge_running(), variant: "default" as const };
  }
  return { label: m.proxy_service_badge_stopped(), variant: "secondary" as const };
}

export function AgentNodeCard({
  config,
  status,
  requestState,
  message,
  apiKeyVisible,
  onConfigChange,
  onToggleApiKey,
  onRefresh,
  onSave,
  onStart,
  onStop,
  onRestart,
}: AgentNodeCardProps) {
  const isWorking = requestState === "working";
  const isRunning = status?.state === "running";
  const badge = resolveBadge(status, message);
  const errorMessage = message || status?.last_error || "";

  return (
    <Card>
      <CardHeader>
        <CardTitle>{m.agent_node_title()}</CardTitle>
        <CardDescription>{m.agent_node_desc()}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">{m.proxy_service_state_label()}</span>
            <Badge variant={badge.variant}>{badge.label}</Badge>
          </div>
          <Button type="button" variant="outline" size="icon" onClick={onRefresh} disabled={isWorking}>
            <RefreshCw className={cn("size-4", isWorking && "animate-spin")} aria-hidden="true" />
            <span className="sr-only">{m.common_refresh()}</span>
          </Button>
        </div>

        <div className="flex items-center justify-between gap-4 py-1">
          <div className="space-y-1">
            <Label htmlFor="agent-node-enabled">{m.common_enabled()}</Label>
            <p className="text-xs text-muted-foreground">{m.agent_node_enabled_help()}</p>
          </div>
          <Switch
            id="agent-node-enabled"
            checked={config.enabled}
            onCheckedChange={(enabled) => onConfigChange({ enabled })}
          />
        </div>

        <div className="grid gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="agent-node-server-url">{m.agent_node_server_url_label()}</Label>
            <Input
              id="agent-node-server-url"
              value={config.server_url}
              placeholder="https://agent.example.com"
              onChange={(event) => onConfigChange({ server_url: event.target.value })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="agent-node-hostname">{m.agent_node_hostname_label()}</Label>
            <Input
              id="agent-node-hostname"
              value={config.hostname ?? ""}
              placeholder="desk-1"
              onChange={(event) => onConfigChange({ hostname: event.target.value })}
            />
            <p className="text-xs text-muted-foreground">{m.agent_node_hostname_help()}</p>
          </div>
        </div>

        <div className="space-y-2">
          <Label htmlFor="agent-node-api-key">{m.agent_node_api_key_label()}</Label>
          <PasswordInput
            id="agent-node-api-key"
            value={config.api_key}
            placeholder="acn_..."
            visible={apiKeyVisible}
            onVisibilityChange={onToggleApiKey}
            onChange={(event) => onConfigChange({ api_key: event.target.value })}
          />
        </div>

        {errorMessage ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-xs text-destructive">
            {errorMessage}
          </div>
        ) : null}

        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" onClick={onSave} disabled={isWorking}>
            {isWorking ? <Loader2 className="animate-spin" aria-hidden="true" /> : null}
            {m.common_save()}
          </Button>
          <Button type="button" variant="outline" onClick={onStart} disabled={isWorking || isRunning}>
            <Play aria-hidden="true" />
            {m.proxy_service_start()}
          </Button>
          <Button type="button" variant="outline" onClick={onStop} disabled={isWorking || !isRunning}>
            <Square aria-hidden="true" />
            {m.proxy_service_stop()}
          </Button>
          <Button type="button" variant="outline" onClick={onRestart} disabled={isWorking || !isRunning}>
            <RotateCcw aria-hidden="true" />
            {m.proxy_service_restart()}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
