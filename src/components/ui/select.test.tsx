import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

describe("SelectItem", () => {
  it("keeps the selected indicator compact", () => {
    render(
      <Select open defaultValue="today">
        <SelectTrigger aria-label="range">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="today">Today</SelectItem>
        </SelectContent>
      </Select>
    )

    const item = screen.getByRole("option", { name: "Today" })
    expect(item).toHaveClass("pr-4")
    expect(item).not.toHaveClass("pr-8")

    const indicator = item.querySelector('[data-slot="select-item-indicator"]')
    if (indicator === null) {
      throw new Error("Select item indicator missing")
    }

    expect(indicator).toHaveClass("size-2")

    const icon = indicator.querySelector("svg")
    if (icon === null) {
      throw new Error("Select item indicator icon missing")
    }

    expect(icon).toHaveClass("size-2")
  })
})
