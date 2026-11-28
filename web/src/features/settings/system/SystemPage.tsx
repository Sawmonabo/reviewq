import { useState } from "react";
import type { ReactNode } from "react";
import type { SystemFacts, SystemMode, UninstallReceipt } from "../../../api/generated/contracts";
import { readSystem, setLoginStart } from "../../../api/system";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { describeError, formatActivityTime, formatClockTime } from "../../../lib/presentation";
import { Policy, Switch } from "../AgentPreferences";
import { QuitDialog } from "./QuitDialog";
import { UninstallDialog } from "./UninstallDialog";
import { useAppInstall } from "./useAppInstall";
import type { InstallState } from "./install-offer";
import "./system.css";

type LoginChange = {
  readonly value?: boolean;
  readonly pending?: boolean;
  readonly error?: string;
};

const statuses: Record<SystemMode, ReactNode> = {
  installed: "Running · background service",
  foreground: (
    <>
      Running · foreground (<code>reviewq serve</code>)
    </>
  ),
  validation: (
    <>
      Running · test server (<code>reviewq serve --validation</code>)
    </>
  ),
};

const desktopLines: Record<InstallState, string> = {
  offered: "Opens in its own window with its own icon, so you never type the address.",
  standalone: "You’re using the ReviewQ app.",
  installed: "Installed. Open ReviewQ from its icon.",
  chromium:
    "ReviewQ is already installed in this browser, or the browser hasn’t offered it yet. You can also use the install icon in the address bar.",
  other: "Open ReviewQ in Chrome or Edge to install it as an app.",
};

export function SystemPage({
  onUninstall,
}: {
  readonly onUninstall: (receipt: UninstallReceipt) => void;
}) {
  const facts = useResource("system", readSystem);
  const [login, setLogin] = useState<LoginChange>({});
  const [dialog, setDialog] = useState<"quit" | "uninstall">();
  const system = facts.data;
  const dataSize = system?.dataBytes == null ? undefined : formatBytes(system.dataBytes);
  const startAtLogin = login.value ?? system?.startAtLogin ?? null;

  // Show only what the service manager reported: its read-back, or after a failure a fresh read.
  async function changeLogin(enabled: boolean) {
    if (login.pending) return;
    setLogin({ value: enabled, pending: true });
    try {
      const { data } = await setLoginStart({ enabled });
      setLogin({ value: data.startAtLogin });
    } catch (error) {
      setLogin({ error: describeError(error) });
      facts.retry();
    }
  }

  return (
    <div className="settings-system">
      <section className="settings-section" aria-labelledby="settings-system-facts">
        <h3 id="settings-system-facts">About</h3>
        {system ? (
          <Facts system={system} dataSize={dataSize} />
        ) : facts.pending ? (
          <Loading label="Reading ReviewQ’s status…" />
        ) : (
          <Notice retry={facts.retry}>{facts.error}</Notice>
        )}
      </section>
      <DesktopApp />
      {system && (
        <section className="settings-section" aria-labelledby="settings-system-service">
          <h3 id="settings-system-service">Background service</h3>
          <div className="settings-agent-policies">
            {system.mode === "installed" ? (
              <>
                <Policy
                  title="Start ReviewQ when I log in"
                  description="Starts ReviewQ in the background each time you log in."
                  notice={login.error && <Notice>{login.error}</Notice>}
                >
                  {startAtLogin !== null && (
                    <Switch
                      label="Start ReviewQ when I log in"
                      checked={startAtLogin}
                      onChange={(enabled) => void changeLogin(enabled)}
                    />
                  )}
                </Policy>
                <Policy
                  title="Quit ReviewQ"
                  description="Stops monitoring until you start it again."
                >
                  <button type="button" onClick={() => setDialog("quit")}>
                    Quit
                  </button>
                </Policy>
              </>
            ) : (
              <div className="settings-policy">
                <span className="meta">
                  Startup, quit and uninstall are available in the installed app.
                </span>
              </div>
            )}
          </div>
        </section>
      )}
      {system?.mode === "installed" && (
        <section className="settings-section" aria-labelledby="settings-system-uninstall">
          <h3 id="settings-system-uninstall">Uninstall</h3>
          <div className="settings-agent-policies">
            <Policy
              title="Uninstall ReviewQ"
              description="Removes the background service and its Claude and Codex add-ons."
            >
              <button
                type="button"
                className="settings-danger"
                onClick={() => setDialog("uninstall")}
              >
                Uninstall
              </button>
            </Policy>
          </div>
        </section>
      )}
      {dialog === "quit" && (
        <QuitDialog startAtLogin={startAtLogin === true} onClose={() => setDialog(undefined)} />
      )}
      {dialog === "uninstall" && system && (
        <UninstallDialog
          dataFolder={system.dataFolder}
          dataSize={dataSize}
          onClose={() => setDialog(undefined)}
          onUninstall={onUninstall}
        />
      )}
    </div>
  );
}

function Facts({
  system,
  dataSize,
}: {
  readonly system: SystemFacts;
  readonly dataSize: string | undefined;
}) {
  const rows = [
    ["Version", system.version],
    ["Status", statuses[system.mode]],
    ["Address", system.address],
    ["Data", dataSize ? `${dataSize} in ${system.dataFolder}` : system.dataFolder],
    ["Last refresh", system.lastRefreshAt ? formatRefresh(system.lastRefreshAt) : "Unknown time"],
  ] as const;
  return (
    <dl className="settings-agent-policies settings-facts">
      {rows.map(([label, value]) => (
        <div key={label}>
          <dt>{label}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function DesktopApp() {
  const { state, install } = useAppInstall();
  return (
    <section className="settings-section" aria-labelledby="settings-system-desktop">
      <h3 id="settings-system-desktop">Desktop app</h3>
      <div className="settings-agent-policies">
        <Policy title="Install ReviewQ as an app" description={desktopLines[state]}>
          {state === "offered" && (
            <button type="button" onClick={install}>
              Install app
            </button>
          )}
        </Policy>
      </div>
    </section>
  );
}

const byteUnits = ["kilobyte", "megabyte", "gigabyte", "terabyte"] as const;

function formatBytes(bytes: number): string {
  if (bytes < 1000) {
    return new Intl.NumberFormat(undefined, {
      style: "unit",
      unit: "byte",
      unitDisplay: "long",
    }).format(bytes);
  }
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < byteUnits.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return new Intl.NumberFormat(undefined, {
    style: "unit",
    unit: byteUnits[unit],
    unitDisplay: "short",
    maximumFractionDigits: 1,
  }).format(value);
}

function formatRefresh(value: string): string {
  return new Date(value).toDateString() === new Date().toDateString()
    ? formatClockTime(value)
    : formatActivityTime(value);
}
