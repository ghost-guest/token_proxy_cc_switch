import { describe, expect, it } from "vitest";

import {
  toPricingRows,
  toPricingSettingsInput,
  type ModelPricingFormRow,
} from "@/features/pricing/form";
import type { ModelPricingSettings } from "@/features/pricing/types";

const settings: ModelPricingSettings = {
  version: "2026-05-16.providerless-v2",
  models: [
    {
      modelId: "gpt-5.5",
      aliases: ["openai/gpt-5.5", "gpt-5.5-latest"],
      short: {
        inputNanoUsdPerToken: 5_000,
        cachedInputNanoUsdPerToken: 500,
        outputNanoUsdPerToken: 30_000,
      },
      long: {
        inputNanoUsdPerToken: 10_000,
        cachedInputNanoUsdPerToken: 1_000,
        outputNanoUsdPerToken: 45_000,
      },
      longContextInputTokenThreshold: 272_000,
    },
  ],
};

describe("pricing/form", () => {
  it("converts nano usd per token to usd per million token fields", () => {
    const rows = toPricingRows(settings);

    expect(rows[0]).toMatchObject({
      modelId: "gpt-5.5",
      aliasesText: "openai/gpt-5.5, gpt-5.5-latest",
      shortInputUsdPerMillion: "5.000",
      shortCachedUsdPerMillion: "0.500",
      shortOutputUsdPerMillion: "30.000",
      longEnabled: true,
      longInputUsdPerMillion: "10.000",
      longCachedUsdPerMillion: "1.000",
      longOutputUsdPerMillion: "45.000",
      longContextInputTokenThreshold: "272000",
    });
  });

  it("converts edited rows back to nano usd per token settings", () => {
    const rows = toPricingRows(settings);
    const result = toPricingSettingsInput(rows);

    expect(result).toEqual({
      ok: true,
      input: {
        models: [
          {
            modelId: "gpt-5.5",
            aliases: ["openai/gpt-5.5", "gpt-5.5-latest"],
            short: {
              inputNanoUsdPerToken: 5_000,
              cachedInputNanoUsdPerToken: 500,
              outputNanoUsdPerToken: 30_000,
            },
            long: {
              inputNanoUsdPerToken: 10_000,
              cachedInputNanoUsdPerToken: 1_000,
              outputNanoUsdPerToken: 45_000,
            },
            longContextInputTokenThreshold: 272_000,
          },
        ],
      },
    });
  });

  it("rejects duplicate aliases across rows", () => {
    const row: ModelPricingFormRow = toPricingRows(settings)[0];
    const result = toPricingSettingsInput([
      row,
      {
        ...row,
        id: "second",
        modelId: "other",
        aliasesText: " gpt-5.5-latest ",
      },
    ]);

    expect(result.ok).toBe(false);
  });

  it("rejects duplicate providerless lookup keys across rows", () => {
    const row: ModelPricingFormRow = toPricingRows(settings)[0];
    const result = toPricingSettingsInput([
      row,
      {
        ...row,
        id: "second",
        modelId: "openai/gpt-5.5",
        aliasesText: "",
      },
    ]);

    expect(result.ok).toBe(false);
  });

  it("keeps provider-prefixed aliases that add a lookup key", () => {
    const row: ModelPricingFormRow = {
      ...toPricingRows(settings)[0],
      aliasesText: "gpt-5.5, openai/gpt-5.5",
    };
    const result = toPricingSettingsInput([row]);

    expect(result).toEqual({
      ok: true,
      input: {
        models: [
          expect.objectContaining({
            modelId: "gpt-5.5",
            aliases: ["openai/gpt-5.5"],
          }),
        ],
      },
    });
  });

  it("normalizes known vendor spelling variants for duplicate checks", () => {
    const row: ModelPricingFormRow = toPricingRows(settings)[0];
    const result = toPricingSettingsInput([
      {
        ...row,
        modelId: "claude-opus-4-7",
        aliasesText: "",
      },
      {
        ...row,
        id: "second",
        modelId: "anthropic/claude-opus-4.7",
        aliasesText: "",
      },
    ]);

    expect(result.ok).toBe(false);
  });

  it("parses decimal prices with big.js precision", () => {
    const row: ModelPricingFormRow = {
      ...toPricingRows(settings)[0],
      id: "decimal-row",
      modelId: "kimi-k2.6",
      aliasesText: "kimi-k2.6, moonshotai/kimi-k2.6",
      shortInputUsdPerMillion: "0.750",
      shortCachedUsdPerMillion: "0.150",
      shortOutputUsdPerMillion: "3.500",
      longEnabled: false,
    };
    const result = toPricingSettingsInput([row]);

    expect(result).toEqual({
      ok: true,
      input: {
        models: [
          {
            modelId: "kimi-k2.6",
            aliases: ["moonshotai/kimi-k2.6"],
            short: {
              inputNanoUsdPerToken: 750,
              cachedInputNanoUsdPerToken: 150,
              outputNanoUsdPerToken: 3500,
            },
            long: null,
            longContextInputTokenThreshold: null,
          },
        ],
      },
    });
  });
});
