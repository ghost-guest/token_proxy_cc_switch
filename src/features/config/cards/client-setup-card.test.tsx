import { describe, expect, it } from "vitest";

import { deriveClientSetupFromForm } from "@/features/config/cards/client-setup-card";
import { EMPTY_FORM, createEmptyUpstream, createModelMapping } from "@/features/config/form";
import type { ClientSetupInfo } from "@/features/config/cards/client-setup-state";

const BASE_SETUP: ClientSetupInfo = {
  proxy_http_base_url: "http://127.0.0.1:19208",
  claude_settings_path: "C:/Users/me/.claude/settings.json",
  claude_base_url: "http://127.0.0.1:19208",
  claude_model: "claude-sonnet-4.5",
  claude_auth_token_configured: true,
  codex_config_path: "C:/Users/me/.codex/config.toml",
  codex_auth_path: "C:/Users/me/.codex/auth.json",
  codex_disable_response_storage: true,
  codex_model: "old-model",
  codex_model_provider: "company-claude-relay",
  codex_model_reasoning_effort: "xhigh",
  codex_network_access: "enabled",
  codex_preferred_auth_method: "apikey",
  codex_provider_base_url: "http://127.0.0.1:19208/v1",
  codex_provider_name: "company-claude-relay",
  codex_provider_requires_openai_auth: false,
  codex_provider_wire_api: "responses",
  codex_api_key_configured: true,
};

function upstream(id: string, priority: string, enabled: boolean, model: string) {
  return {
    ...createEmptyUpstream(),
    id,
    priority,
    enabled,
    modelMappings: [createModelMapping(model, model)],
  };
}

describe("deriveClientSetupFromForm", () => {
  it("overlays Codex model and provider from the currently selected upstream", () => {
    const form = {
      ...EMPTY_FORM,
      upstreams: [
        upstream("company-claude-relay", "10", false, "claude-sonnet-4.6"),
        upstream("tokenskingdom-openai-responses", "20", true, "gpt-5.5"),
      ],
    };

    const setup = deriveClientSetupFromForm(BASE_SETUP, form);

    expect(setup?.codex_model).toBe("gpt-5.5");
    expect(setup?.codex_model_provider).toBe("tokenskingdom-openai-responses");
    expect(setup?.codex_provider_name).toBe("tokenskingdom-openai-responses");
  });

  it("falls back to backend preview when no upstream is selected", () => {
    const form = {
      ...EMPTY_FORM,
      upstreams: [upstream("tokenskingdom-openai-responses", "20", false, "gpt-5.5")],
    };

    expect(deriveClientSetupFromForm(BASE_SETUP, form)).toBe(BASE_SETUP);
  });
});
