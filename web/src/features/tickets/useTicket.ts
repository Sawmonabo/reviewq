import { useEffect, useEffectEvent, useRef, useState } from "react";
import { ApiResponseError } from "../../api/client";
import { watchResources } from "../../api/events";
import type { TicketDetail } from "../../api/generated/contracts/tickets";
import { readTicket, readTicketComments } from "../../api/tickets";
import type { TicketCommentOrder } from "../../api/tickets";
import { describeError } from "../../lib/presentation";

type Request = {
  readonly order: TicketCommentOrder;
  readonly trail: readonly string[];
  readonly scope?: string;
};
type Result = {
  readonly request: Request;
  readonly ticket: TicketDetail;
};

export function useTicket(ticketKey: string) {
  const [request, setRequest] = useState<Request>({ order: "newest", trail: [] });
  const [result, setResult] = useState<Result>();
  const [failure, setFailure] = useState<{ request: Request; message: string }>();
  const active = useRef<AbortController>(null);
  const subscriptions = useRef<readonly ReturnType<typeof watchResources>[]>([]);
  const readPage = useEffectEvent(async ({ order, trail, scope }: Request, signal: AbortSignal) => {
    if (!scope) return readTicket(ticketKey, order, signal);
    const detail = result?.ticket;
    if (detail?.commentScope !== scope)
      throw new ApiResponseError("conflict", "The returned ticket changed.", false);
    const { data, cursor } = await readTicketComments(
      ticketKey,
      order,
      trail.at(-1),
      scope,
      signal,
    );
    const ticket = {
      ...detail,
      items: data.items,
      nextCursor: data.nextCursor,
      commentCount: data.commentCount,
      fetchedAt: data.fetchedAt,
    };
    return { data: ticket, cursor };
  });
  const resume = useEffectEvent(() => {
    active.current?.abort();
    setRequest((old) =>
      result?.request === old ? { ...old, scope: result.ticket.commentScope } : { ...old },
    );
  });

  useEffect(() => {
    function invalidateAuthority() {
      active.current?.abort();
      setResult(undefined);
      setRequest((old) => ({ order: old.order, trail: [] }));
    }
    function hide() {
      active.current?.abort();
    }
    function restore(event: PageTransitionEvent) {
      if (event.persisted) invalidateAuthority();
    }
    const settings = watchResources([{ kind: "settings" }], (reason) => {
      if (reason !== "visible") invalidateAuthority();
    });
    const ticket = watchResources([{ kind: "ticket", id: ticketKey }], (reason) => {
      if (reason === "visible") resume();
      if (reason === "invalidate") invalidateAuthority();
    });
    subscriptions.current = [settings, ticket];
    window.addEventListener("pagehide", hide);
    window.addEventListener("pageshow", restore);
    return () => {
      settings.close();
      ticket.close();
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("pageshow", restore);
    };
  }, [ticketKey]);

  useEffect(() => {
    const controller = new AbortController();
    active.current = controller;
    const leases = subscriptions.current;
    void readPage(request, controller.signal).then(
      ({ data, cursor }) => {
        if (controller.signal.aborted) return;
        setResult({ request, ticket: data });
        setFailure(undefined);
        // Only an opening refreshes the detail protected by these subscriptions.
        if (!request.scope) for (const lease of leases) lease.snapshot(cursor);
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (!canKeepTicket(error)) setResult(undefined);
        setFailure({ request, message: describeError(error) });
      },
    );
    return () => controller.abort();
  }, [request, ticketKey]);

  const failed = failure?.request === request ? failure : undefined;
  const selection = failed && result ? result.request : request;
  return {
    ticket: result?.ticket,
    order: selection.order,
    trail: selection.trail,
    switching: !failed && result?.request !== request,
    error: failed?.message,
    select: (order: TicketCommentOrder, trail: readonly string[]) => {
      active.current?.abort();
      const scope = result?.ticket.commentScope;
      setRequest({ order, trail, ...(scope && { scope }) });
    },
    retry: () => {
      active.current?.abort();
      setRequest(result ? { ...request } : { order: request.order, trail: [] });
    },
  };
}

function canKeepTicket(error: unknown) {
  return (
    error instanceof TypeError ||
    (error instanceof ApiResponseError &&
      ["busy", "unavailable", "payload_too_large", "internal"].includes(error.code))
  );
}
