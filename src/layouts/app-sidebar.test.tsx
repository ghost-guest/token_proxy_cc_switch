import { cleanup, render, screen } from "@testing-library/react";
import type { ComponentProps, ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SidebarProvider } from "@/components/ui/sidebar";
import { AppSidebar } from "@/layouts/app-sidebar";
import { m } from "@/paraglide/messages.js";
import { setLocale } from "@/paraglide/runtime.js";

const routerState = vi.hoisted(() => ({
  pathname: "/config/dashboard",
}));

type RouterState = {
  location: {
    pathname: string;
  };
};

type UseRouterStateArgs<T> = {
  select?: (state: RouterState) => T;
};

vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children, ...props }: ComponentProps<"a"> & { to: string; children: ReactNode }) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useRouterState: <T,>({ select }: UseRouterStateArgs<T>) => {
    const state: RouterState = { location: { pathname: routerState.pathname } };
    return select ? select(state) : state;
  },
}));

describe("layouts/AppSidebar", () => {
  afterEach(() => {
    cleanup();
    setLocale("en", { reload: false });
    routerState.pathname = "/config/dashboard";
  });

  it("localizes the Agent Node menu item", () => {
    setLocale("zh", { reload: false });
    routerState.pathname = "/agent-node";

    render(
      <SidebarProvider>
        <AppSidebar />
      </SidebarProvider>
    );

    expect(screen.getByRole("link", { name: m.agent_node_title({}, { locale: "zh" }) })).toHaveAttribute(
      "href",
      "/agent-node"
    );
    expect(screen.queryByText("Agent Node")).not.toBeInTheDocument();
  });
});
