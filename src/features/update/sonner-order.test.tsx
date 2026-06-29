import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { Toaster, toast } from "sonner";

function EffectToastBeforeLaterToaster() {
  return (
    <>
      <ToastEffect message="Lost update available" />
      <Toaster />
    </>
  );
}

function ToasterBeforeEffectToast() {
  return (
    <>
      <Toaster />
      <ToastEffect message="Visible update available" />
    </>
  );
}

function ToastEffect({ message }: { message: string }) {
  useEffect(() => {
    toast(message, { duration: Infinity });
  }, [message]);

  return null;
}

describe("sonner root ordering", () => {
  afterEach(() => {
    cleanup();
    toast.dismiss();
  });

  it("does not replay an effect toast emitted before the toaster subscribes", async () => {
    render(<EffectToastBeforeLaterToaster />);

    await new Promise((resolve) => window.setTimeout(resolve, 20));

    expect(screen.queryByText("Lost update available")).not.toBeInTheDocument();
  });

  it("shows an effect toast emitted after the toaster subscribes first", async () => {
    render(<ToasterBeforeEffectToast />);

    await waitFor(() => {
      expect(screen.getByText("Visible update available")).toBeInTheDocument();
    });
  });
});
