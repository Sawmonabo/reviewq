import { useEffect, useEffectEvent, useRef, useState } from "react";
import { ApiResponseError } from "../../api/client";
import { watchResources } from "../../api/events";
import type { TicketDetail } from "../../api/generated/contracts/tickets";
import { readTicket, readTicketComments } from "../../api/tickets";
import { ApiTransportError } from "../../api/transport-error";
import type { TicketCommentOrder } from "../../api/tickets";
import { useWorkspaceRecovery } from "../../hooks/useWorkspaceRecovery";
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
  const [failure, setFailure] = useState<{
    request: Request;
    message: string;
    connection: boolean;
  }>();
  const [retryRevision, setRetryRevision] = useState(0);
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

  function retryConnection() {
    if (!active.current) setRetryRevision((value) => value + 1);
  }

  const resume = useEffectEvent(() => {
    if (!result || (failure?.request === request && failure.connection)) {
      retryConnection();
      return;
    }
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
    function settle() {
      if (active.current === controller) active.current = null;
    }
    void readPage(request, controller.signal).then(
      ({ data, cursor }) => {
        settle();
        if (controller.signal.aborted) return;
        setResult({ request, ticket: data });
        setFailure(undefined);
        // Only an opening refreshes the detail protected by these subscriptions.
        if (!request.scope) for (const lease of leases) lease.snapshot(cursor);
      },
      (error: unknown) => {
        settle();
        if (controller.signal.aborted) return;
        if (!canKeepTicket(error)) setResult(undefined);
        setFailure({
          request,
          message: describeError(error),
          connection: error instanceof ApiTransportError,
        });
      },
    );
    return () => {
      controller.abort();
      settle();
    };
  }, [request, retryRevision, ticketKey]);

  const failed = failure?.request === request ? failure : undefined;
  useWorkspaceRecovery(
    failed?.connection
      ? {
          kind: "read",
          label: `Jira ticket ${ticketKey}`,
          confirmedAt: result?.ticket.fetchedAt,
          retry: retryConnection,
        }
      : undefined,
  );
  const selection = failed && result ? result.request : request;

  return {
    ticket: result?.ticket,
    order: selection.order,
    trail: selection.trail,
    switching: !failed && result?.request !== request,
    error: failed?.connection ? undefined : failed?.message,
    unavailable: Boolean(failed?.connection && !result),
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
    error instanceof ApiTransportError ||
    (error instanceof ApiResponseError &&
      ["busy", "unavailable", "payload_too_large", "internal"].includes(error.code))
  );
}
