"use client"

import * as React from "react"
import { Area, AreaChart, CartesianGrid, XAxis } from "recharts"

import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart"
import {
  createDashboardTimeFormatter,
  formatDashboardTimestamp,
  formatInteger,
} from "@/features/dashboard/format"
import type { DashboardSeriesPoint } from "@/features/dashboard/types"
import { useI18n } from "@/lib/i18n"
import { m } from "@/paraglide/messages.js"

type ChartPoint = {
  tsMs: number
  inputTokens: number
  outputTokens: number
}

const chartConfig = {
  inputTokens: {
    label: m.dashboard_chart_input_tokens(),
    color: "var(--chart-1)",
  },
  outputTokens: {
    label: m.dashboard_chart_output_tokens(),
    color: "var(--chart-2)",
  },
} satisfies ChartConfig

type ChartAreaInteractiveProps = {
  series: DashboardSeriesPoint[]
}

type ChartBodyProps = {
  data: ChartPoint[]
  timeFormatter: Intl.DateTimeFormat
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null
}

function resolveTimestampMs(value: unknown) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value
  }
  if (typeof value === "string") {
    const numeric = Number(value)
    if (Number.isFinite(numeric)) {
      return numeric
    }
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) {
      return parsed
    }
  }
  return null
}

function resolveTooltipTimestamp(payload: unknown) {
  if (!Array.isArray(payload) || payload.length === 0) {
    return null
  }
  const first = payload[0]
  if (!isRecord(first)) {
    return null
  }
  const inner = first.payload
  if (!isRecord(inner)) {
    return null
  }
  // Recharts tooltip label 可能缺失或被格式化，优先使用原始 payload 的时间戳。
  return resolveTimestampMs(inner.tsMs)
}

function formatTick(value: unknown, formatter: Intl.DateTimeFormat) {
  const tsMs = resolveTimestampMs(value)
  if (tsMs === null) {
    return "—"
  }
  return formatDashboardTimestamp(tsMs, formatter)
}

function ChartHeader() {
  return (
    <CardHeader>
      <CardTitle>{m.dashboard_stat_total_tokens()}</CardTitle>
    </CardHeader>
  )
}

function ChartDefs() {
  return (
    <defs>
      <linearGradient id="fillInput" x1="0" y1="0" x2="0" y2="1">
        <stop offset="5%" stopColor="var(--color-inputTokens)" stopOpacity={0.3} />
        <stop offset="95%" stopColor="var(--color-inputTokens)" stopOpacity={0.02} />
      </linearGradient>
      <linearGradient id="fillOutput" x1="0" y1="0" x2="0" y2="1">
        <stop offset="5%" stopColor="var(--color-outputTokens)" stopOpacity={0.28} />
        <stop offset="95%" stopColor="var(--color-outputTokens)" stopOpacity={0.02} />
      </linearGradient>
    </defs>
  )
}

function ChartCanvas({ data, timeFormatter }: ChartBodyProps) {
  return (
    <ChartContainer config={chartConfig} className="aspect-auto h-[250px] w-full">
      <AreaChart data={data}>
        <ChartDefs />
        <CartesianGrid vertical={false} />
        <XAxis
          dataKey="tsMs"
          tickLine={false}
          axisLine={false}
          tickMargin={8}
          minTickGap={24}
          tickFormatter={(value) => formatTick(value, timeFormatter)}
        />
        <ChartTooltip
          cursor={false}
          content={
            <ChartTooltipContent
              labelFormatter={(value, payload) =>
                formatTick(resolveTooltipTimestamp(payload) ?? value, timeFormatter)
              }
              formatter={(value, name) => {
                const label = chartConfig[name as keyof typeof chartConfig]?.label ?? name
                return (
                  <div className="flex min-w-0 items-center gap-2">
                    <span className="text-muted-foreground">{label}</span>
                    <span className="ml-auto font-medium">{formatInteger(Number(value))}</span>
                  </div>
                )
              }}
              indicator="dot"
            />
          }
        />
        <Area
          dataKey="inputTokens"
          type="monotone"
          stroke="var(--color-inputTokens)"
          fill="url(#fillInput)"
          strokeWidth={2}
          dot={false}
        />
        <Area
          dataKey="outputTokens"
          type="monotone"
          stroke="var(--color-outputTokens)"
          fill="url(#fillOutput)"
          strokeWidth={2}
          dot={false}
        />
      </AreaChart>
    </ChartContainer>
  )
}

function ChartBody({ data, timeFormatter }: ChartBodyProps) {
  return (
    <CardContent className="px-2 pt-4 sm:px-6 sm:pt-6">
      {data.length ? (
        <ChartCanvas data={data} timeFormatter={timeFormatter} />
      ) : (
        <div className="flex h-[250px] items-center justify-center text-sm text-muted-foreground">
          {m.dashboard_no_data()}
        </div>
      )}
    </CardContent>
  )
}

export function ChartAreaInteractive({ series }: ChartAreaInteractiveProps) {
  const { locale } = useI18n()
  const timeFormatter = React.useMemo(
    () => createDashboardTimeFormatter(locale),
    [locale]
  )
  const chartData = React.useMemo(
    () =>
      series.map((item) => ({
        tsMs: item.tsMs,
        inputTokens: item.inputTokens,
        outputTokens: item.outputTokens,
      })),
    [series]
  )

  return (
    <Card className="@container/card">
      <ChartHeader />
      <ChartBody data={chartData} timeFormatter={timeFormatter} />
    </Card>
  )
}
