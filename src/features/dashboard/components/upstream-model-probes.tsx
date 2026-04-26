import { AlertTriangle, Ban, CheckCircle2, Clock3 } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import type {
  DashboardUpstreamModelProbe,
  DashboardUpstreamModelProbeStatus,
} from "@/features/dashboard/types"
import { cn } from "@/lib/utils"
import { m } from "@/paraglide/messages.js"

type UpstreamModelProbesProps = {
  probes: DashboardUpstreamModelProbe[]
}

const STATUS_ICON = {
  pending: Clock3,
  ok: CheckCircle2,
  failed: AlertTriangle,
  unsupported: Ban,
} satisfies Record<DashboardUpstreamModelProbeStatus, typeof Clock3>

const STATUS_CLASS_NAME = {
  pending: "text-muted-foreground",
  ok: "text-emerald-600 dark:text-emerald-400",
  failed: "text-destructive",
  unsupported: "text-muted-foreground",
} satisfies Record<DashboardUpstreamModelProbeStatus, string>

const VISIBLE_MODEL_LIMIT = 5

function statusLabel(status: DashboardUpstreamModelProbeStatus) {
  switch (status) {
    case "pending":
      return m.dashboard_upstream_models_status_pending()
    case "ok":
      return m.dashboard_upstream_models_status_ok()
    case "failed":
      return m.dashboard_upstream_models_status_failed()
    case "unsupported":
      return m.dashboard_upstream_models_status_unsupported()
  }
}

function providerLabel(probe: DashboardUpstreamModelProbe) {
  if (probe.accountId) {
    return `${probe.provider} · ${probe.accountId}`
  }
  return probe.provider
}

function ProbeStatus({ status }: { status: DashboardUpstreamModelProbeStatus }) {
  const Icon = STATUS_ICON[status]
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm", STATUS_CLASS_NAME[status])}>
      <Icon className="size-4" aria-hidden="true" />
      <span>{statusLabel(status)}</span>
    </span>
  )
}

function ProbeModels({ models }: { models: string[] }) {
  if (models.length === 0) {
    return <span className="text-muted-foreground text-sm">{m.dashboard_upstream_models_empty()}</span>
  }

  const visibleModels = models.slice(0, VISIBLE_MODEL_LIMIT)
  const hiddenCount = Math.max(0, models.length - visibleModels.length)
  return (
    <div className="flex min-w-0 flex-wrap gap-1.5">
      {visibleModels.map((model) => (
        <Badge key={model} variant="outline" className="max-w-full">
          <span className="truncate">{model}</span>
        </Badge>
      ))}
      {hiddenCount > 0 ? (
        <Badge variant="secondary">
          {m.dashboard_upstream_models_more({ count: hiddenCount })}
        </Badge>
      ) : null}
    </div>
  )
}

function ProbeRow({
  probe,
  index,
}: {
  probe: DashboardUpstreamModelProbe
  index: number
}) {
  return (
    <div
      className="grid gap-3 border-t py-4 first:border-t-0 first:pt-0 last:pb-0 md:grid-cols-[minmax(9rem,1fr)_minmax(8rem,12rem)_minmax(0,2fr)]"
      data-testid={`upstream-model-probe-${index}`}
    >
      <div className="min-w-0">
        <div className="truncate text-sm font-medium">{probe.upstreamId}</div>
        <div className="text-muted-foreground truncate text-xs">{providerLabel(probe)}</div>
      </div>
      <div className="flex min-w-0 flex-col gap-1">
        <ProbeStatus status={probe.status} />
        <span className="text-muted-foreground text-xs">
          {m.dashboard_upstream_models_count({ count: probe.models.length })}
        </span>
      </div>
      <div className="min-w-0 space-y-2">
        <ProbeModels models={probe.models} />
        {probe.error ? (
          <p className="text-destructive line-clamp-2 text-xs">
            {m.dashboard_upstream_models_error({ error: probe.error })}
          </p>
        ) : null}
      </div>
    </div>
  )
}

export function UpstreamModelProbes({ probes }: UpstreamModelProbesProps) {
  return (
    <div className="px-4 lg:px-6">
      <Card>
        <CardHeader>
          <CardTitle>{m.dashboard_upstream_models_title()}</CardTitle>
          <CardDescription>{m.dashboard_upstream_models_desc()}</CardDescription>
        </CardHeader>
        <CardContent>
          {probes.length > 0 ? (
            probes.map((probe, index) => (
              <ProbeRow
                key={`${probe.provider}:${probe.upstreamId}:${probe.accountId ?? "public"}:${index}`}
                probe={probe}
                index={index}
              />
            ))
          ) : (
            <div className="text-muted-foreground text-sm">
              {m.dashboard_upstream_models_empty()}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
