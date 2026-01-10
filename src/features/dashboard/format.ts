const DASHBOARD_TIME_FORMAT_OPTIONS: Intl.DateTimeFormatOptions = {
  dateStyle: "short",
  timeStyle: "medium",
};

export function createDashboardTimeFormatter(locale: string) {
  return new Intl.DateTimeFormat(locale, DASHBOARD_TIME_FORMAT_OPTIONS);
}

export function formatDashboardTimestamp(tsMs: number, formatter: Intl.DateTimeFormat) {
  const date = new Date(tsMs);
  return Number.isNaN(date.getTime()) ? "—" : formatter.format(date);
}

// 使用逗号作为千位分隔符，便于阅读
export function formatInteger(value: number) {
  return Math.round(value).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}
