import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { UpstreamEditorFields } from "@/features/config/cards/upstreams/editor-dialog-form";
import { createEmptyUpstream } from "@/features/config/form";
import { m } from "@/paraglide/messages.js";

afterEach(() => {
  cleanup();
});

describe("upstreams/editor-dialog-form", () => {
  it("renders kiro account selector when provider is kiro", () => {
    const draft = createEmptyUpstream();
    draft.id = "kiro-default";
    draft.providers = ["kiro"];

    render(
      <UpstreamEditorFields
        draft={draft}
        providerOptions={["kiro"]}
        appProxyUrl=""
        showApiKeys={false}
        onToggleApiKeys={vi.fn()}
        onChangeDraft={vi.fn()}
      />
    );

    expect(screen.queryByText(m.field_kiro_account())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_base_url())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_proxy_url())).not.toBeInTheDocument();
    expect(screen.getByLabelText(m.field_id())).toBeDisabled();
    expect(screen.getByRole("button", { name: /kiro/i })).toBeDisabled();
  });

  it("renders codex account selector when provider is codex", () => {
    const draft = createEmptyUpstream();
    draft.id = "codex-default";
    draft.providers = ["codex"];

    render(
      <UpstreamEditorFields
        draft={draft}
        providerOptions={["codex"]}
        appProxyUrl=""
        showApiKeys={false}
        onToggleApiKeys={vi.fn()}
        onChangeDraft={vi.fn()}
      />
    );

    expect(screen.queryByText(m.field_codex_account())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_base_url())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_proxy_url())).not.toBeInTheDocument();
    expect(screen.getByLabelText(m.field_id())).toBeDisabled();
    expect(screen.getByRole("button", { name: /codex/i })).toBeDisabled();
  });

  it("hides network and api key fields when provider is antigravity", () => {
    const draft = createEmptyUpstream();
    draft.id = "antigravity-default";
    draft.providers = ["antigravity"];

    render(
      <UpstreamEditorFields
        draft={draft}
        providerOptions={["antigravity"]}
        appProxyUrl=""
        showApiKeys={false}
        onToggleApiKeys={vi.fn()}
        onChangeDraft={vi.fn()}
      />
    );

    expect(screen.queryByLabelText(m.field_base_url())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_proxy_url())).not.toBeInTheDocument();
    expect(screen.queryByLabelText(m.field_api_key())).not.toBeInTheDocument();
    expect(screen.getByLabelText(m.field_id())).toBeEnabled();
    expect(screen.getByRole("button", { name: /antigravity/i })).toBeEnabled();
  });
});
