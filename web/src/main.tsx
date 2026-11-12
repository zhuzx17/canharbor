// SPDX-License-Identifier: AGPL-3.0-or-later
import { formatNumber, formatTime } from "./i18n/format";
import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  api,
  ApiFailure,
  validateFrame,
  type Auth,
  type BusEvent,
  type Device,
  type Frame,
  type Session,
} from "./api";
import { CapturePanel } from "./CapturePanel";
import {
  describeConfiguration,
  sameConfiguration,
  sessionConfiguration,
  type EditableConfiguration,
} from "./configuration";
import { decode } from "./contract";
import { ErrorRecovery } from "./ErrorRecovery";
import type {
  Capabilities,
  CreateRequest,
  SessionMode,
  Subscribe,
} from "./generated/api";
import { HardwareGuide } from "./HardwareGuide";
import { initializeLanguage, useTranslation } from "./i18n";
import {
  displayMessage,
  LocalizedError,
  stateText,
  text,
  uiMessage,
  type UiMessage,
} from "./i18n/messages";
import { LanguageSwitcher } from "./LanguageSwitcher";
import { aggregateFrames, MAX_AGGREGATES, type Aggregate } from "./monitor";
import { OfflineCapturePanel } from "./OfflineCapturePanel";
import { StressPanel } from "./StressPanel";
import "./style.css";
import { TaskPanel } from "./TaskPanel";
import { TransmissionActions } from "./TransmissionActions";
type FrameBusEvent = Extract<BusEvent, { type: "frame_received" }>;
const MAX_ROWS = 500;
// Capture once, before React effects. The code never reaches storage or request URLs.
const pairCode = new URLSearchParams(location.hash.slice(1)).get("pair");
if (pairCode) history.replaceState(null, "", location.pathname);
const views = ["live", "capture", "offline", "stress"];
const initialView = views.includes(location.hash.slice(1))
  ? location.hash.slice(1)
  : null;
const draftKey = "canharbor.workbench.v1";
function readDraft(): Record<string, unknown> {
  try {
    const raw = localStorage.getItem(draftKey);
    if (!raw || raw.length > 8192) return {};
    const value: unknown = JSON.parse(raw);
    return value && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}
const savedDraft = readDraft();
const draftString = (key: string, fallback: string) =>
  typeof savedDraft[key] === "string"
    ? (savedDraft[key] as string).slice(0, 2048)
    : fallback;
function App() {
  const { t } = useTranslation();
  const [workspace, setWorkspace] = useState(
    initialView ??
      (["live", "capture", "offline", "stress"].includes(
        draftString("workspace", "live"),
      )
        ? draftString("workspace", "live")
        : "live"),
  );
  function navigate(view: string) {
    setWorkspace(view);
    if (location.hash !== `#${view}`) history.pushState(null, "", `#${view}`);
  }
  useEffect(() => {
    const restore = () => {
      const view = location.hash.slice(1);
      if (views.includes(view)) setWorkspace(view);
    };
    addEventListener("popstate", restore);
    addEventListener("hashchange", restore);
    return () => {
      removeEventListener("popstate", restore);
      removeEventListener("hashchange", restore);
    };
  }, []);
  const [expandSettings, setExpandSettings] = useState(false);
  const [recordingFile, setRecordingFile] = useState<File | null>(null);
  const [auth, setAuth] = useState<Auth | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedId, setSelectedId] = useState(draftString("selectedId", ""));
  const [channel, setChannel] = useState("virtual:0");
  const [mode, setMode] = useState<SessionMode>("listen_only");
  const [nominalBitrate, setNominalBitrate] = useState(500000);
  const [fdEnabled, setFdEnabled] = useState(false);
  const [dataBitrate, setDataBitrate] = useState(2000000);
  const selectedDevice = devices.find((device) =>
    device.channels.some((item) => item.id === channel),
  );
  const selectedChannel = selectedDevice?.channels.find(
    (item) => item.id === channel,
  );
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [capabilityError, setCapabilityError] = useState<unknown>(null);
  useEffect(() => {
    let active = true;
    setCapabilities(null);
    setCapabilityError("");
    if (auth && channel) {
      void api(
        "capabilities",
        `/channels/${encodeURIComponent(channel)}/capabilities`,
      )
        .then((value) => {
          if (active) setCapabilities(value);
        })
        .catch(() => {
          if (active)
            setCapabilityError(new LocalizedError("main:validation_0"));
        });
    }
    return () => {
      active = false;
    };
  }, [auth, channel]);
  const [error, setError] = useState<unknown>(null);
  const pairAttempted = useRef(false);
  const [lastQuery, setLastQuery] = useState<number | null>(null);
  const [queryStale, setQueryStale] = useState(true);
  const queryRevision = useRef(0);
  const [lastRenewal, setLastRenewal] = useState<number | null>(null);
  const [renewalUnknown, setRenewalUnknown] = useState(false);
  const [streamSegments, setStreamSegments] = useState(0);
  const monitoredSession = useRef<string | null>(null);
  const [connectionBusy, setConnectionBusy] = useState(true);
  const [notice, setNotice] = useState<UiMessage>("");
  const [busy, setBusy] = useState(false);
  const [streamState, setStreamState] = useState<
    "disconnected" | "connecting" | "live"
  >("disconnected");
  const [streamRevision, setStreamRevision] = useState(0);
  const [idFilter, setIdFilter] = useState(draftString("idFilter", ""));
  const [directionFilter, setDirectionFilter] = useState(
    ["all", "rx", "tx"].includes(draftString("directionFilter", "all"))
      ? draftString("directionFilter", "all")
      : "all",
  );
  const [rows, setRows] = useState<FrameBusEvent[]>([]);
  const [received, setReceived] = useState(0);
  const [monitorView, setMonitorView] = useState("frames");
  const [aggregates, setAggregates] = useState<Aggregate[]>([]);
  const aggregateRef = useRef(new Map<string, Aggregate>());
  const displayEvicted = useRef(0);
  const rowCount = useRef(0);
  const groupEvicted = useRef(0);
  const pausedSkipped = useRef(0);
  const [displayCounts, setDisplayCounts] = useState({
    evicted: 0,
    groups: 0,
    paused: 0,
  });
  const [diagnostics, setDiagnostics] = useState<
    Extract<BusEvent, { type: "bus_diagnostic" }>[]
  >([]);
  const [gaps, setGaps] = useState(0n);
  const [paused, setPaused] = useState(false);
  const pausedRef = useRef(false);
  const pending = useRef<FrameBusEvent[]>([]);
  const receivedRef = useRef(0);
  const elapsedOrigin = useRef<string | null>(null);
  const [kind, setKind] = useState<Frame["kind"]>(
    ["classic_data", "classic_remote", "fd_data"].includes(
      draftString("kind", "classic_data"),
    )
      ? (draftString("kind", "classic_data") as Frame["kind"])
      : "classic_data",
  );
  const [id, setId] = useState(draftString("id", "123"));
  const [data, setData] = useState(draftString("data", "01 02 03 04"));
  const [extended, setExtended] = useState(savedDraft.extended === true);
  const [brs, setBrs] = useState(savedDraft.brs === true);
  const [dlc, setDlc] = useState(
    typeof savedDraft.dlc === "number" &&
      Number.isInteger(savedDraft.dlc) &&
      savedDraft.dlc >= 0 &&
      savedDraft.dlc <= 8
      ? savedDraft.dlc
      : 0,
  );
  // A page must explicitly acquire control, even if the shared cookie already owns a lease.
  const [lease, setLease] = useState<{
    id: string;
    generation: string;
    renewAt: number;
  } | null>(null);
  useEffect(() => {
    try {
      localStorage.setItem(
        draftKey,
        JSON.stringify({
          workspace,
          selectedId,
          idFilter,
          directionFilter,
          kind,
          id,
          data,
          extended,
          brs,
          dlc,
        }),
      );
    } catch {
      /* Storage may be unavailable. */
    }
  }, [
    workspace,
    selectedId,
    idFilter,
    directionFilter,
    kind,
    id,
    data,
    extended,
    brs,
    dlc,
  ]);
  const leaseRef = useRef(lease);
  leaseRef.current = lease;
  const authRef = useRef(auth);
  authRef.current = auth;
  async function checkStatus() {
    setConnectionBusy(true);
    try {
      if (!authRef.current) {
        const current = await api("auth", "/auth/session");
        setAuth(current);
      }
      await refresh();
      const result = await api("devices", "/devices");
      setDevices(result.devices);
      setError(null);
    } catch (e) {
      setQueryStale(true);
      reportError(e);
    } finally {
      setConnectionBusy(false);
    }
  }
  const session = sessions.find((s) => s.id === selectedId);
  const [configBase, setConfigBase] = useState<{
    id: string;
    value: EditableConfiguration;
  } | null>(null);
  const draftConfig: EditableConfiguration = {
    mode,
    nominalBitrate: selectedDevice?.simulated ? 500000 : nominalBitrate,
    fdEnabled,
    dataBitrate: selectedDevice?.simulated ? 2000000 : dataBitrate,
  };
  const appliedConfig = session ? sessionConfiguration(session) : null;
  const configDirty =
    !!appliedConfig && !sameConfiguration(draftConfig, appliedConfig);
  const configConflict =
    !!appliedConfig &&
    !!configBase &&
    configBase.id === session?.id &&
    !sameConfiguration(configBase.value, appliedConfig);
  function loadConfiguration(value: EditableConfiguration) {
    setMode(value.mode);
    setNominalBitrate(value.nominalBitrate);
    setFdEnabled(value.fdEnabled);
    setDataBitrate(value.dataBitrate);
    if (session) setConfigBase({ id: session.id, value });
  }
  useEffect(() => {
    if (!session || !appliedConfig) {
      setConfigBase(null);
      return;
    }
    if (
      !configBase ||
      configBase.id !== session.id ||
      sameConfiguration(draftConfig, configBase.value)
    ) {
      if (
        !configBase ||
        configBase.id !== session.id ||
        !sameConfiguration(configBase.value, appliedConfig)
      )
        loadConfiguration(appliedConfig);
    }
  }, [
    session?.id,
    session?.config.mode,
    session?.config.nominal_bitrate,
    session?.config.data_bitrate,
  ]);
  const controls =
    !!session &&
    !!lease &&
    lease.id === session.id &&
    lease.generation === session.generation &&
    session.controller === auth?.client_id;
  const validNominal =
    selectedDevice?.simulated ||
    (Number.isInteger(nominalBitrate) &&
      nominalBitrate >= 1 &&
      nominalBitrate <= 10000000);
  const validData =
    !fdEnabled ||
    selectedDevice?.simulated ||
    (Number.isInteger(dataBitrate) &&
      dataBitrate >= 1 &&
      dataBitrate <= 20000000);
  const rateIssue =
    capabilities &&
    (capabilities.fixed_nominal_bitrates.length > 0 &&
    !capabilities.fixed_nominal_bitrates.includes(draftConfig.nominalBitrate)
      ? text("main:validation_1")
      : fdEnabled &&
          capabilities.fixed_data_bitrates.length > 0 &&
          !capabilities.fixed_data_bitrates.includes(draftConfig.dataBitrate)
        ? text("main:validation_2")
        : "");
  const configValid = validNominal && validData && !rateIssue;
  let frameError = "";
  try {
    validateFrame(kind, id, extended, data, brs, dlc);
    if (
      kind === "fd_data" &&
      (!selectedChannel?.fd || session?.config.data_bitrate == null)
    )
      frameError = text("main:validation_3");
  } catch (e) {
    frameError = e instanceof Error ? e.message : String(e);
  }
  const normalizedId = idFilter
    .trim()
    .replace(/^0x/i, "")
    .replace(/^0+(?=.)/, "")
    .toUpperCase();
  const invalidFilter = !!normalizedId && !/^[0-9A-F]{1,8}$/.test(normalizedId);
  const matchesFilter = (row: FrameBusEvent) =>
    !invalidFilter &&
    (!normalizedId ||
      row.frame.id.toString(16).toUpperCase() === normalizedId) &&
    (directionFilter === "all" || row.direction === directionFilter);
  const visibleRows = rows.filter(matchesFilter);
  const visibleAggregates = aggregates.filter((group) =>
    matchesFilter(group.latest),
  );
  const sendBlocked = !auth
    ? text("main:validation_4")
    : renewalUnknown
      ? text("main:validation_5")
      : !session
        ? text("main:validation_6")
        : !controls
          ? session.controller
            ? text("main:validation_7")
            : text("main:validation_8")
          : session.mode !== "active"
            ? text("main:validation_9")
            : session.state !== "running"
              ? text("main:validation_10")
              : streamState !== "live"
                ? text("main:validation_11")
                : busy
                  ? text("main:validation_12")
                  : frameError;
  async function refreshDevices() {
    const result = await api("devices", "/devices");
    setDevices(result.devices);
    if (!result.devices.some((d) => d.channels.some((c) => c.id === channel))) {
      if (lease) {
        setNotice(
          uiMessage(
            "main:the_selected_device_is_unavailable_close_its_session_before",
          ),
        );
        return;
      }
      setChannel(result.devices.flatMap((d) => d.channels)[0]?.id ?? "");
      setSelectedId("");
      setLease(null);
      setFdEnabled(false);
      setNominalBitrate(500000);
      setDataBitrate(2000000);
    }
  }
  async function refresh() {
    const revision = queryRevision.current;
    const queriedLease = leaseRef.current;
    const result = await api("sessions", "/sessions");
    // A read started before stream loss cannot acknowledge that newer loss.
    if (revision !== queryRevision.current) return;
    setSessions(result.sessions);
    setLastQuery(Date.now());
    setQueryStale(false);
    const owned = queriedLease;
    if (owned && leaseRef.current === queriedLease) {
      const current = result.sessions.find((item) => item.id === owned.id);
      if (
        !current ||
        current.generation !== owned.generation ||
        current.controller !== authRef.current?.client_id
      )
        setLease(null);
    }
  }
  function reportError(error: unknown) {
    setError(error);
    if (error instanceof ApiFailure && error.status === 401) {
      setAuth(null);
      setLease(null);
    } else if (error instanceof ApiFailure && error.status === 403) {
      setLease(null);
    }
  }
  async function act(action: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await refresh();
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  }
  useEffect(() => {
    let alive = true;
    setConnectionBusy(true);
    (async () => {
      try {
        const usePair = !!pairCode && !pairAttempted.current;
        pairAttempted.current = true;
        const current = usePair
          ? await api("auth", "/auth/pair", "POST", { code: pairCode })
          : await api("auth", "/auth/session");
        if (!alive) return;
        setAuth(current);
        const result = await api("devices", "/devices");
        if (!alive) return;
        setDevices(result.devices);
        await refresh();
      } catch (e) {
        if (alive) reportError(e);
      } finally {
        if (alive) setConnectionBusy(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);
  useEffect(() => {
    if (!auth) return;
    const timer = setInterval(() => {
      refresh().catch((e) => {
        setQueryStale(true);
        reportError(e);
      });
    }, 2000);
    return () => clearInterval(timer);
  }, [auth]);
  useEffect(() => {
    if (!auth || !lease) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    setRenewalUnknown(false);
    const renew = async () => {
      const started = performance.now();
      try {
        await api(
          "session",
          `/sessions/${lease.id}/lease/renew`,
          "POST",
          {},
          auth,
          lease.generation,
        );
        if (active) {
          setLastRenewal(Date.now());
          setRenewalUnknown(false);
        }
      } catch (e) {
        if (active) {
          reportError(e);
          if (
            e instanceof ApiFailure &&
            [401, 403, 404, 409, 412, 428].includes(e.status)
          )
            setLease(null);
          else setRenewalUnknown(true);
        }
        return;
      }
      // Anchor each renewal to its request, not a possibly delayed response.
      // Keep one request in flight and never reacquire an expired lease.
      if (active)
        timer = setTimeout(
          renew,
          Math.max(0, started + 5000 - performance.now()),
        );
    };
    timer = setTimeout(renew, Math.max(0, lease.renewAt - performance.now()));
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [auth, lease]);
  useEffect(() => {
    if (monitoredSession.current !== selectedId) {
      monitoredSession.current = selectedId;
      setLastRenewal(null);
      pending.current = [];
      elapsedOrigin.current = null;
      pausedRef.current = false;
      setPaused(false);
      setRows([]);
      receivedRef.current = 0;
      setReceived(0);
      aggregateRef.current.clear();
      setAggregates([]);
      displayEvicted.current = 0;
      rowCount.current = 0;
      groupEvicted.current = 0;
      pausedSkipped.current = 0;
      setDisplayCounts({ evicted: 0, groups: 0, paused: 0 });
      setGaps(0n);
      setDiagnostics([]);
      setStreamSegments(0);
    } else if (selectedId && auth) {
      setStreamSegments((n) => n + 1);
    }
    if (!auth || !selectedId) {
      setStreamState("disconnected");
      return;
    }
    let active = true;
    const socket = new WebSocket(
      `${location.origin.replace("http:", "ws:")}/api/v1/sessions/${selectedId}/stream`,
    );
    setStreamState("connecting");
    socket.onopen = () =>
      socket.send(
        JSON.stringify({
          type: "subscribe",
          protocol_version: "1.0",
          csrf_token: auth.csrf_token,
          filter: {},
        } satisfies Subscribe),
      );
    socket.onmessage = (event) => {
      try {
        const message = decode("stream_message", JSON.parse(event.data));
        if (message.type === "hello") setStreamState("live");
        else if (message.type === "gap") {
          setGaps((n) => n + BigInt(message.count));
        } else if (message.type === "events") {
          const batch = message.events.filter(
            (item): item is FrameBusEvent => item.type === "frame_received",
          );
          const diagnosticBatch = message.events.filter(
            (item): item is Extract<BusEvent, { type: "bus_diagnostic" }> =>
              item.type === "bus_diagnostic",
          );
          if (diagnosticBatch.length)
            setDiagnostics((old) => [...old, ...diagnosticBatch].slice(-64));
          if (elapsedOrigin.current === null && batch.length)
            elapsedOrigin.current = batch[0].host_monotonic_ns;
          receivedRef.current += batch.length;
          if (!pausedRef.current) {
            groupEvicted.current += aggregateFrames(
              aggregateRef.current,
              batch,
            );
            displayEvicted.current += Math.max(
              0,
              pending.current.length + batch.length - MAX_ROWS,
            );
            pending.current = [...pending.current, ...batch].slice(-MAX_ROWS);
          } else pausedSkipped.current += batch.length;
        } else if (message.type === "session_fault") {
          setError(
            new LocalizedError("main:channelFault", { code: message.code }),
          );
          setLease(null);
          socket.close();
        } else if (message.type === "resume_unavailable") {
          setNotice(
            uiMessage(
              "main:stream_history_is_unavailable_select_the_session_again_to",
            ),
          );
          socket.close();
        } else {
          const unhandled: never = message;
          throw new LocalizedError("main:unsupported_stream_message_value", {
            unhandled: unhandled,
          });
        }
      } catch {
        setError(new LocalizedError("main:validation_13"));
        socket.close();
      }
    };
    socket.onclose = () => {
      if (active) {
        queryRevision.current++;
        setStreamState("disconnected");
        setQueryStale(true);
        setNotice(
          uiMessage(
            "main:stream_closed_reconnect_explicitly_transmission_is_not_resumed",
          ),
        );
      }
    };
    socket.onerror = () => {
      if (active) setError(new LocalizedError("main:validation_14"));
    };
    const flush = setInterval(() => {
      setReceived(receivedRef.current);
      setDisplayCounts({
        evicted: displayEvicted.current,
        groups: groupEvicted.current,
        paused: pausedSkipped.current,
      });
      if (pending.current.length) {
        const batch = pending.current;
        pending.current = [];
        displayEvicted.current += Math.max(
          0,
          rowCount.current + batch.length - MAX_ROWS,
        );
        rowCount.current = Math.min(MAX_ROWS, rowCount.current + batch.length);
        setRows((old) => [...old, ...batch].slice(-MAX_ROWS));
        setAggregates([...aggregateRef.current.values()]);
      }
    }, 100);
    return () => {
      active = false;
      clearInterval(flush);
      socket.close();
    };
  }, [selectedId, auth, streamRevision]);
  function togglePause() {
    pausedRef.current = !paused;
    setPaused(!paused);
    if (!paused) pausedSkipped.current += pending.current.length;
    pending.current = [];
  }
  async function control(path: string) {
    if (!session || !lease || !auth) return;
    await api(
      "session",
      `/sessions/${session.id}/${path}`,
      "POST",
      {},
      auth,
      lease.generation,
    );
  }
  return (
    <div className="app">
      <aside className="sidebar">
        <a className="brand" href="/">
          CAN<span>Harbor</span>
          <small>{text("main:local_workbench")}</small>
        </a>
        <div className="stage">{text("main:local_can_workbench")}</div>
        <nav>
          <a
            href="#live"
            className={workspace === "live" ? "active" : ""}
            onClick={(e) => {
              e.preventDefault();
              navigate("live");
            }}
          >
            {text("main:workspace")}
            <span>01</span>
          </a>
          <a href="#devices" onClick={() => setWorkspace("live")}>
            {text("main:devices")}
            <span>{devices.length}</span>
          </a>
          <a
            href="#capture"
            className={workspace === "capture" ? "active" : ""}
            onClick={(e) => {
              e.preventDefault();
              navigate("capture");
            }}
          >
            {t("recordings")}
          </a>
          <a
            href="#offline"
            className={workspace === "offline" ? "active" : ""}
            onClick={(e) => {
              e.preventDefault();
              navigate("offline");
            }}
          >
            {t("offline")}
          </a>
          <a
            href="#stress"
            className={workspace === "stress" ? "active" : ""}
            onClick={(e) => {
              e.preventDefault();
              navigate("stress");
            }}
          >
            {t("stress")}
          </a>
          <a href="#about">{t("about")}</a>
        </nav>
        <LanguageSwitcher auth={auth} />
        <div className="side-footer">
          <span className={`dot ${auth ? "connected" : ""}`} />
          <span>
            {auth ? text("main:browser_paired") : text("main:pairing_required")}
          </span>
          <p>{text("main:local_access_no_cloud_connection")}</p>
        </div>
      </aside>
      <main id="workspace">
        <header>
          <div>
            <div className="eyebrow">{text("main:can_can_fd_platform")}</div>
            <h1>{t("workspace")}</h1>
            <p>
              {text(
                "main:inspect_traffic_and_send_frames_through_the_local_rust",
              )}
            </p>
          </div>
          <span className="simulation">
            <span className="dot" />
            {devices.length === 0
              ? text("main:no_device_status_yet")
              : devices.every((device) => device.simulated)
                ? text("main:simulation_only")
                : text("main:hardware_access_enabled_unverified")}
          </span>
        </header>
        {session && (workspace === "offline" || workspace === "stress") && (
          <aside
            className="active-channel"
            aria-label={text("main:active_channel_summary")}
          >
            <p>
              {session.channel_id} ·{" "}
              {queryStale
                ? text("main:status_uncertain")
                : stateText(session.state)}{" "}
              ·{" "}
              {controls
                ? text("main:you_have_control")
                : text("main:read_only")}
              {text("main:stream")} {stateText(streamState)}
            </p>
            {controls && (
              <button
                disabled={busy}
                onClick={() =>
                  void act(async () => {
                    await api(
                      "empty",
                      `/sessions/${selectedId}/lease`,
                      "DELETE",
                      {},
                      auth!,
                      lease!.generation,
                    );
                    setLease(null);
                    setNotice(
                      uiMessage("main:channel_stopped_and_control_released"),
                    );
                  })
                }
              >
                {text("main:stop_and_release_live_channel")}
              </button>
            )}
          </aside>
        )}
        <nav
          aria-label={text("main:workbench_views")}
          className="button-row workbench-views"
        >
          {[
            ["live", text("main:live_traffic")],
            ["capture", text("main:recordings")],
            ["offline", text("main:offline_analysis")],
            ["stress", text("main:stress_testing")],
          ].map(([value, label]) => (
            <button
              key={value}
              aria-pressed={workspace === value}
              onClick={() => navigate(value)}
            >
              {label}
            </button>
          ))}
        </nav>
        {diagnostics.length > 0 && (
          <section aria-label={text("main:bus_diagnostics")}>
            <h2>{text("main:bus_diagnostics")}</h2>
            <p role="status">
              {diagnostics.length}
              {text(
                "main:recent_bus_diagnostic_events_inspect_details_for_error_classes",
              )}
            </p>
            <details>
              <summary>{text("main:diagnostic_details")}</summary>
              <ul>
                {diagnostics.map((item) => (
                  <li key={item.sequence}>
                    {item.overflow
                      ? text(
                          "main:receive_overflow_observations_may_be_missing",
                        )
                      : item.error_classes & 0x40
                        ? text(
                            "main:bus_off_inspect_the_bus_before_explicitly_restarting",
                          )
                        : text(
                            "main:bus_diagnostic_inspect_adapter_details",
                          )}{" "}
                    {text("main:classes_0x")}
                    {item.error_classes.toString(16)} · {item.raw_data_hex}
                    {item.overflow
                      ? text("main:receive_loss_reported_count_unknown")
                      : ""}{" "}
                    · {item.timestamp_quality}
                  </li>
                ))}
              </ul>
            </details>
          </section>
        )}
        {!!error && (
          <ErrorRecovery
            error={error}
            busy={busy || connectionBusy}
            onDismiss={() => setError(null)}
            onCheck={() => {
              void checkStatus();
            }}
          />
        )}
        {!auth && (
          <section className="pair-panel">
            <h2>{text("main:pair_this_browser")}</h2>
            <p>
              {text("main:start_the_local_service_then_run")}
              <code>canharbor web open</code>
              {text("main:pairing_links_expire_after_60_seconds")}
            </p>
          </section>
        )}
        <section
          id="devices"
          className={`connection ${session?.state === "running" ? "running" : ""} ${expandSettings || configDirty || configConflict ? "expanded" : ""}`}
          hidden={workspace === "offline" || workspace === "stress"}
        >
          <p className="next-step" role="status">
            <strong>{text("main:next_step")}</strong>
            {!auth
              ? text("main:pair_this_browser_using_canharbor_web_open")
              : !session
                ? text("main:choose_a_channel_and_mode_then_open_channel")
                : !controls
                  ? session.controller
                    ? text(
                        "main:this_session_is_controlled_elsewhere_wait_for_release_or",
                      )
                    : text(
                        "main:acquire_control_to_start_or_configure_this_channel",
                      )
                  : session.state !== "running"
                    ? text("main:review_settings_then_start_channel")
                    : session.mode === "listen_only"
                      ? text(
                          "main:receiving_in_listen_only_mode_send_from_the_other",
                        )
                      : text(
                          "main:channel_running_use_send_once_or_record_traffic",
                        )}
          </p>
          <div className="section-label">{text("main:01_connection")}</div>

          <button
            disabled={!auth || busy}
            onClick={() => void act(refreshDevices)}
          >
            {text("main:refresh_devices")}
          </button>
          {!devices.length && (
            <p>
              {text(
                "main:no_registered_channels_found_use_the_adapter_guide_to",
              )}
            </p>
          )}
          {session?.state === "running" && (
            <button
              aria-expanded={expandSettings}
              onClick={() => setExpandSettings(!expandSettings)}
            >
              {expandSettings
                ? text("main:hide_settings")
                : text("main:show_channel_settings")}
            </button>
          )}
          <HardwareGuide />
          <details className="virtual-guide">
            <summary>
              {text("main:first_virtual_receive_send_walkthrough")}
            </summary>
            <ol>
              <li>
                {text(
                  "main:choose_virtual_1_and_listen_only_then_open_channel",
                )}
              </li>
              <li>
                {text("main:in_a_terminal_run")}{" "}
                <code>
                  canharbor send --channel virtual:0 --id 123 --data 01020304
                  --active
                </code>
                .
              </li>
              <li>{text("main:the_monitor_shows_id_123_with_bytes_01_02")}</li>
            </ol>
            <p>
              {text(
                "main:no_background_frames_are_generated_each_action_is_explicit",
              )}
            </p>
          </details>
          {session && appliedConfig && (
            <section
              aria-label={text("main:session_configuration")}
              className="session-configuration"
            >
              <p>
                <strong>{text("main:applied_configuration")}</strong>{" "}
                {describeConfiguration(appliedConfig)}
              </p>
              {configDirty && (
                <p>
                  {text(
                    "main:unapplied_changes_the_channel_still_uses_the_applied_configuration",
                  )}
                </p>
              )}
              {configConflict && (
                <p role="alert">
                  {text(
                    "main:the_server_configuration_changed_while_you_were_editing_review",
                  )}
                </p>
              )}
              <button
                disabled={busy || (!configDirty && !configConflict)}
                onClick={() => loadConfiguration(appliedConfig)}
              >
                {text("main:discard_draft_and_load_applied_settings")}
              </button>
              {configConflict && (
                <button
                  disabled={busy}
                  onClick={() =>
                    setConfigBase({ id: session.id, value: appliedConfig })
                  }
                >
                  {text("main:keep_draft_against_current_configuration")}
                </button>
              )}
            </section>
          )}
          <div className="connection-grid">
            <label>
              {text("main:channel")}
              <select
                value={channel}
                disabled={!!lease}
                title={
                  lease
                    ? text(
                        "main:stop_and_release_control_or_close_the_controlled_session",
                      )
                    : undefined
                }
                onChange={(e) => {
                  setChannel(e.target.value);
                  setSelectedId("");
                  setLease(null);
                  setFdEnabled(false);
                  setNominalBitrate(500000);
                  setDataBitrate(2000000);
                }}
              >
                {devices
                  .flatMap((d) => d.channels)
                  .map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.label} · {c.id}
                    </option>
                  ))}
              </select>
            </label>
            <label>
              {text("main:mode")}
              <select
                value={mode}
                onChange={(e) => setMode(e.target.value as SessionMode)}
              >
                <option
                  value="listen_only"
                  disabled={selectedChannel?.listen_only === false}
                >
                  {text("main:listen_only")}
                </option>
                <option value="active">
                  {text("main:active_allows_transmission")}
                </option>
              </select>
            </label>
            <div className="fixed-setting">
              <label>
                {text("main:nominal_bitrate")}
                <input
                  aria-label={text("main:nominal_bitrate")}
                  aria-invalid={!validNominal}
                  aria-describedby={!validNominal ? "nominal-error" : undefined}
                  type="number"
                  list="nominal-rates"
                  min="1"
                  max="10000000"
                  disabled={selectedDevice?.simulated}
                  value={selectedDevice?.simulated ? 500000 : nominalBitrate}
                  onChange={(e) => setNominalBitrate(Number(e.target.value))}
                />
              </label>
              <datalist id="nominal-rates">
                {(capabilities?.fixed_nominal_bitrates.length
                  ? capabilities.fixed_nominal_bitrates
                  : [125000, 250000, 500000, 1000000]
                ).map((rate) => (
                  <option key={rate} value={rate}>
                    {rate / 1000}
                    {text("main:kbit_s")}
                  </option>
                ))}
              </datalist>
              {!validNominal && (
                <p id="nominal-error">
                  {text(
                    "main:enter_a_whole_nominal_bitrate_from_1_to_10000000",
                  )}
                </p>
              )}
              <label>
                <input
                  type="checkbox"
                  checked={fdEnabled}
                  disabled={!selectedChannel?.fd}
                  onChange={(e) => setFdEnabled(e.target.checked)}
                />
                {text("main:enable_can_fd")}
              </label>
              {fdEnabled && (
                <label>
                  {text("main:fd_data_bitrate")}
                  <input
                    aria-label={text("main:fd_data_bitrate")}
                    aria-invalid={!validData}
                    aria-describedby={
                      !validData ? "data-bitrate-error" : undefined
                    }
                    type="number"
                    list="data-rates"
                    min="1"
                    max="20000000"
                    disabled={selectedDevice?.simulated}
                    value={selectedDevice?.simulated ? 2000000 : dataBitrate}
                    onChange={(e) => setDataBitrate(Number(e.target.value))}
                  />
                </label>
              )}
              <datalist id="data-rates">
                {(capabilities?.fixed_data_bitrates.length
                  ? capabilities.fixed_data_bitrates
                  : [1000000, 2000000, 4000000, 8000000]
                ).map((rate) => (
                  <option key={rate} value={rate}>
                    {rate / 1000000}
                    {text("main:mbit_s")}
                  </option>
                ))}
              </datalist>
              {rateIssue && <p role="alert">{rateIssue}</p>}
              {!!capabilityError && (
                <p>
                  {capabilityError instanceof Error
                    ? capabilityError.message
                    : String(capabilityError)}
                </p>
              )}
              {capabilities && (
                <details>
                  <summary>{text("main:device_timing_capabilities")}</summary>
                  <p>
                    {capabilities.fixed_nominal_bitrates.length
                      ? text("main:supported_nominal_rates_value_bit_s", {
                          value: capabilities.fixed_nominal_bitrates.join(", "),
                        })
                      : text(
                          "main:suggested_rates_are_not_a_compatibility_claim_the_device",
                        )}
                  </p>
                  {capabilities.nominal_timing && (
                    <p>
                      {text("main:nominal_clock")}
                      {capabilities.nominal_timing.clock_hz}
                      {text("main:hz_prescaler")}
                      {capabilities.nominal_timing.brp_min}–
                      {capabilities.nominal_timing.brp_max}
                      {text("main:step")} {capabilities.nominal_timing.brp_inc}
                      {text("main:segment_1")}{" "}
                      {capabilities.nominal_timing.tseg1_min}–
                      {capabilities.nominal_timing.tseg1_max}
                      {text("main:segment_2")}{" "}
                      {capabilities.nominal_timing.tseg2_min}–
                      {capabilities.nominal_timing.tseg2_max}.
                    </p>
                  )}
                  {fdEnabled && (
                    <p>
                      {capabilities.fixed_data_bitrates.length
                        ? text("main:supported_fd_data_rates_value_bit_s", {
                            value: capabilities.fixed_data_bitrates.join(", "),
                          })
                        : text(
                            "main:fd_rates_require_device_timing_validation",
                          )}
                    </p>
                  )}
                </details>
              )}
              {!validData && (
                <p id="data-bitrate-error">
                  {text("main:enter_a_whole_data_bitrate_from_1_to_20000000")}
                </p>
              )}
              <small>
                {selectedDevice?.simulated
                  ? text("main:virtual_bus_fixed_500_kbit_s_2_mbit_s")
                  : text("main:hardware_compatibility_has_not_been_verified")}
              </small>
            </div>
            <button
              disabled={
                !auth || busy || !selectedChannel || !!session || !configValid
              }
              className="primary"
              onClick={() =>
                act(async () => {
                  const created = await api(
                    "session",
                    "/sessions",
                    "POST",
                    {
                      channel_id: channel,
                      config: {
                        mode,
                        nominal_bitrate: selectedDevice?.simulated
                          ? 500000
                          : nominalBitrate,
                        ...(fdEnabled
                          ? {
                              fd: {
                                enabled: true,
                                data_bitrate: selectedDevice?.simulated
                                  ? 2000000
                                  : dataBitrate,
                                iso: true,
                              },
                            }
                          : {}),
                      },
                    } satisfies CreateRequest,
                    auth!,
                  );
                  setSelectedId(created.id);
                  setNotice(
                    uiMessage(
                      "main:session_created_acquire_control_then_start_the_channel",
                    ),
                  );
                })
              }
            >
              {text("main:open_channel")}
            </button>
          </div>
          <div className="session-bar">
            <label>
              {text("main:session")}
              <select
                aria-label={text("main:session")}
                disabled={!!lease}
                title={
                  lease
                    ? text(
                        "main:stop_and_release_control_or_close_the_controlled_session_2",
                      )
                    : undefined
                }
                value={selectedId}
                onChange={(e) => {
                  setLease(null);
                  setSelectedId(e.target.value);
                  const selected = sessions.find(
                    (item) => item.id === e.target.value,
                  );
                  if (selected) {
                    setChannel(selected.channel_id);
                    setMode(selected.config.mode);
                    setNominalBitrate(selected.config.nominal_bitrate);
                    setFdEnabled(selected.config.data_bitrate !== null);
                    setDataBitrate(selected.config.data_bitrate ?? 2000000);
                  }
                }}
              >
                <option value="">{text("main:select_a_session")}</option>
                {sessions.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.id} · {s.channel_id} · {stateText(s.mode)}
                  </option>
                ))}
              </select>
            </label>
            <span className="session-state">
              {session ? stateText(session.state) : text("main:no_session")}
            </span>
            <details
              className="status-details"
              open={queryStale || renewalUnknown}
            >
              <summary>{text("main:connection_and_control_details")}</summary>
              <p data-testid="channel-status-certainty">
                {queryStale || !lastQuery
                  ? text(
                      "main:current_channel_state_unknown_last_reported_value_a_disconnected",
                      { value: stateText(session?.state ?? "unavailable") },
                    )
                  : text("main:last_reported_channel_state_value", {
                      value: stateText(session?.state ?? "unavailable"),
                    })}
              </p>
              <p>
                {text("main:last_successful_status_query")}{" "}
                {lastQuery
                  ? formatTime(new Date(lastQuery))
                  : text("main:not_yet_confirmed")}
                {text("main:last_successful_control_renewal")}{" "}
                {lastRenewal
                  ? formatTime(new Date(lastRenewal))
                  : text("main:not_yet_confirmed")}
                .
              </p>
              {selectedId && streamState === "disconnected" && (
                <p>
                  {text(
                    "main:event_stream_disconnected_the_channel_may_remain_running_while",
                  )}
                </p>
              )}
              {renewalUnknown && (
                <p role="alert">
                  {text(
                    "main:control_renewal_outcome_unknown_renewal_is_paused_the_channel",
                  )}
                </p>
              )}
              <button
                disabled={connectionBusy || !auth}
                onClick={() => void checkStatus()}
              >
                {text("main:check_final_status")}
              </button>
            </details>
            <span className="control-state">
              {controls
                ? text("main:you_have_control")
                : session?.controller
                  ? text("main:controlled_by_another_page_or_cli")
                  : text("main:read_only")}
            </span>
            <button
              disabled={
                !controls ||
                busy ||
                session?.state !== "configured" ||
                configConflict ||
                !configValid
              }
              onClick={() =>
                act(async () => {
                  const updated = await api(
                    "session",
                    `/sessions/${selectedId}/config`,
                    "PATCH",
                    {
                      mode,
                      nominal_bitrate: nominalBitrate,
                      ...(fdEnabled
                        ? {
                            fd: {
                              enabled: true,
                              data_bitrate: dataBitrate,
                              iso: true,
                            },
                          }
                        : {}),
                    },
                    auth!,
                    lease!.generation,
                    session!.revision,
                  );
                  loadConfiguration(sessionConfiguration(updated));
                  setNotice(
                    uiMessage("main:configuration_updated_revision_value", {
                      revision: updated.revision,
                    }),
                  );
                })
              }
            >
              {text("main:apply_settings_to")}
              {session?.channel_id ?? text("main:selected_session")}
            </button>
            <div className="button-row">
              <button
                disabled={!session || !!session.controller || busy}
                onClick={() =>
                  act(async () => {
                    const requested = performance.now();
                    const owned = await api(
                      "session",
                      `/sessions/${selectedId}/lease`,
                      "POST",
                      {},
                      auth!,
                    );
                    setLease({
                      id: owned.id,
                      generation: owned.generation,
                      renewAt: requested + 5000,
                    });
                  })
                }
              >
                {text("main:acquire_control")}
              </button>
              <button
                disabled={!controls || busy}
                onClick={() =>
                  act(() =>
                    control(session?.state === "running" ? "stop" : "start"),
                  )
                }
              >
                {session?.state === "running"
                  ? text("main:stop_channel")
                  : text("main:start_channel")}
              </button>
              <button
                disabled={!controls || busy}
                onClick={() =>
                  void act(async () => {
                    await api(
                      "empty",
                      `/sessions/${selectedId}/lease`,
                      "DELETE",
                      {},
                      auth!,
                      lease!.generation,
                    );
                    setLease(null);
                    setNotice(
                      uiMessage(
                        "main:channel_stopped_and_control_released_the_session_remains_available",
                      ),
                    );
                  })
                }
              >
                {text("main:stop_and_release_control")}
              </button>
              <button
                disabled={!controls || busy}
                onClick={() =>
                  act(async () => {
                    await api(
                      "empty",
                      `/sessions/${selectedId}`,
                      "DELETE",
                      {},
                      auth!,
                      lease!.generation,
                    );
                    setLease(null);
                    setSelectedId("");
                  })
                }
              >
                {text("main:close")}
              </button>
            </div>
            <small className="stop-impact">
              {text(
                "main:stopping_or_releasing_stops_acquisition_for_all_observers_and",
              )}
            </small>
          </div>
        </section>
        {notice && (
          <p
            role="status"
            aria-label={text("main:operation_result")}
            className="notice"
          >
            {displayMessage(notice)}
          </p>
        )}
        <div hidden={workspace !== "capture"}>
          {auth && (
            <CapturePanel
              auth={auth}
              session={session}
              onInspect={(file) => {
                setRecordingFile(file);
                navigate("offline");
              }}
            />
          )}
        </div>
        <div hidden={workspace !== "stress"}>
          {auth && <StressPanel auth={auth} devices={devices} />}
        </div>
        <div hidden={workspace !== "offline"}>
          <OfflineCapturePanel recordingFile={recordingFile} />
        </div>
        <div
          className="work-grid"
          hidden={workspace !== "live"}
          style={workspace !== "live" ? { display: "none" } : undefined}
        >
          <section className="monitor">
            <div className="panel-heading">
              <div>
                <div className="section-label">{text("main:02_traffic")}</div>
                <h2>
                  {text("main:frame_monitor")}{" "}
                  <span
                    className={`stream ${streamState === "live" ? "live" : ""}`}
                  >
                    {
                      {
                        disconnected: text("main:disconnected"),
                        connecting: text("main:connecting"),
                        live: text("main:live"),
                      }[streamState]
                    }
                  </span>
                </h2>
              </div>
              <div className="button-row">
                <button
                  disabled={!session || streamState === "live" || busy}
                  onClick={() => setStreamRevision((n) => n + 1)}
                >
                  {text("main:reconnect_stream")}
                </button>
                <button disabled={!session} onClick={togglePause}>
                  {paused
                    ? text("main:resume_display")
                    : text("main:pause_display")}
                </button>
                <button
                  onClick={() => {
                    pending.current = [];
                    rowCount.current = 0;
                    setRows([]);
                    aggregateRef.current.clear();
                    setAggregates([]);
                  }}
                >
                  {text("main:clear")}
                </button>
              </div>
            </div>
            {streamSegments > 0 && (
              <p data-testid="stream-segments">
                {text("main:stream_reconnected")}
                {streamSegments}
                {text(
                  "main:time_s_traffic_during_disconnection_is_unavailable_retained_rows",
                )}
              </p>
            )}
            <div className="monitor-stats">
              <div>
                <span>{text("main:received")}</span>
                <strong>{formatNumber(received)}</strong>
              </div>
              <div>
                <span>{text("main:stream_gaps")}</span>
                <strong>{formatNumber(gaps)}</strong>
              </div>
              <div>
                <span>{text("main:display_buffer")}</span>
                <strong>
                  {rows.length} <small>/ {MAX_ROWS}</small>
                </strong>
              </div>
              <div>
                <span>{text("main:display")}</span>
                <strong>
                  {paused ? text("main:paused") : text("main:following")}
                </strong>
              </div>
            </div>
            <div className="monitor-filters">
              <label>
                {text("main:monitor_view")}
                <select
                  aria-label={text("main:monitor_view")}
                  value={monitorView}
                  onChange={(e) => setMonitorView(e.target.value)}
                >
                  <option value="frames">{text("main:recent_frames")}</option>
                  <option value="ids">
                    {text("main:by_id_retained_counts")}
                  </option>
                </select>
              </label>
              <label>
                {text("main:filter_id_hex")}
                <input
                  value={idFilter}
                  onChange={(e) => setIdFilter(e.target.value)}
                  placeholder="123"
                  aria-invalid={invalidFilter}
                  aria-describedby="id-filter-help"
                />
              </label>
              <label>
                {text("main:direction")}
                <select
                  value={directionFilter}
                  onChange={(e) => setDirectionFilter(e.target.value)}
                >
                  <option value="all">{text("main:all_directions")}</option>
                  <option value="rx">RX</option>
                  <option value="tx">TX</option>
                </select>
              </label>
            </div>
            <p id="id-filter-help">
              {text(
                "main:exact_id_match_with_optional_0x_prefix_recent_frames",
              )}{" "}
              {MAX_ROWS}
              {text("main:frames_by_id_retains_up_to")}
              {MAX_AGGREGATES}
              {text(
                "main:groups_across_the_observed_stream_pausing_discards_display_updates",
              )}
            </p>
            {invalidFilter && (
              <p role="alert">
                {text("main:enter_up_to_eight_hexadecimal_digits_for_an_exact")}
              </p>
            )}
            <p className="display-loss">
              {text("main:display_evicted")}
              {formatNumber(displayCounts.evicted)}
              {text("main:frames_paused_updates_skipped")}
              {formatNumber(displayCounts.paused)}
              {text("main:id_groups_evicted")}
              {formatNumber(displayCounts.groups)}
              {text("main:these_are_separate_from_stream_gaps")}
            </p>
            {monitorView === "ids" && (
              <div className="table-scroll aggregate-table">
                <table>
                  <thead>
                    <tr>
                      <th>{text("main:id_hex")}</th>
                      <th>{text("main:width")}</th>
                      <th>{text("main:direction")}</th>
                      <th>{text("main:type")}</th>
                      <th>{text("main:observed_count")}</th>
                      <th>{text("main:latest_data_hex")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {visibleAggregates.map((group) => (
                      <tr key={group.key}>
                        <td>
                          {group.latest.frame.id.toString(16).toUpperCase()}
                        </td>
                        <td>
                          {group.latest.frame.extended
                            ? text("main:29_bit")
                            : text("main:11_bit")}
                        </td>
                        <td>{group.latest.direction.toUpperCase()}</td>
                        <td>{group.latest.frame.kind}</td>
                        <td>{formatNumber(group.count)}</td>
                        <td>
                          {group.latest.frame.data_hex
                            .match(/.{2}/g)
                            ?.join(" ") || "—"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                {!visibleAggregates.length && (
                  <p>
                    {text(
                      "main:no_id_groups_match_counts_start_at_subscription_or",
                    )}
                  </p>
                )}
              </div>
            )}
            <div className="table-scroll" hidden={monitorView !== "frames"}>
              <table>
                <thead>
                  <tr>
                    <th>{text("main:sequence")}</th>
                    <th>{text("main:elapsed_s")}</th>
                    <th>{text("main:dir")}</th>
                    <th>{text("main:id_hex")}</th>
                    <th>{text("main:type")}</th>
                    <th>DLC</th>
                    <th>{text("main:data_hex")}</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleRows.map((row, index) => (
                    <tr key={`${row.sequence}-${index}`}>
                      <td>{row.sequence}</td>
                      <td
                        title={text("main:value_reported_value_s_epoch_value", {
                          timestamp_quality: row.timestamp_quality,
                          value: row.device_raw_timestamp_us ?? "unavailable",
                          timestamp_epoch: row.timestamp_epoch,
                        })}
                      >
                        {(
                          Number(
                            (BigInt(row.host_monotonic_ns) -
                              BigInt(
                                elapsedOrigin.current ?? row.host_monotonic_ns,
                              )) /
                              1000000n,
                          ) / 1000
                        ).toFixed(3)}
                      </td>
                      <td>
                        <span className={`direction ${row.direction}`}>
                          {row.direction.toUpperCase()}
                        </span>
                      </td>
                      <td className="frame-id">
                        {row.frame.id
                          .toString(16)
                          .toUpperCase()
                          .padStart(row.frame.extended ? 8 : 3, "0")}
                      </td>
                      <td>
                        {row.frame.kind === "fd_data"
                          ? "FD"
                          : row.frame.kind === "classic_remote"
                            ? "RTR"
                            : "CAN"}
                        {row.frame.brs ? text("main:brs") : ""}
                        {row.frame.esi ? text("main:esi") : ""}
                      </td>
                      <td>{row.frame.dlc}</td>
                      <td>
                        {row.frame.data_hex.match(/.{2}/g)?.join(" ") || "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {!!rows.length && !visibleRows.length && (
                <div className="empty">
                  <p>{text("main:no_frames_match_the_current_filters")}</p>
                  <button
                    onClick={() => {
                      setIdFilter("");
                      setDirectionFilter("all");
                    }}
                  >
                    {text("main:clear_filters")}
                  </button>
                </div>
              )}
              {!rows.length && (
                <div className="empty">
                  <div className="empty-lines">┄ ┄ ┄</div>
                  <h3>{text("main:waiting_for_traffic")}</h3>
                  <p>
                    {text(
                      "main:start_this_channel_then_send_from_the_other_virtual",
                    )}
                    <br />
                    {text("main:no_background_frames_are_generated")}
                  </p>
                </div>
              )}
            </div>
            <div className="table-footer">
              <span>{text("main:time_relative_to_first_received_frame")}</span>
              <span>
                {text("main:last")}
                {MAX_ROWS}
                {text("main:frames_display_is_independent_of_reception")}
              </span>
            </div>
          </section>
          <section className="transmit">
            <div className="section-label">{text("main:03_transmit")}</div>
            <h2>{text("main:send_a_frame")}</h2>
            <p className="muted">
              {text("main:single_frame_submission_to_the_selected_channel")}
            </p>
            <label>
              {text("main:frame_type")}
              <select
                value={kind}
                onChange={(e) => {
                  setKind(e.target.value as Frame["kind"]);
                  setBrs(false);
                  if (e.target.value === "classic_remote") setData("");
                }}
              >
                <option value="classic_data">{text("main:classic_can")}</option>
                <option value="fd_data">CAN FD</option>
                <option value="classic_remote">
                  {text("main:classic_rtr")}
                </option>
              </select>
            </label>
            <label>
              {text("main:identifier")}
              <small>{text("main:hexadecimal")}</small>
              <input
                value={id}
                onChange={(e) => setId(e.target.value)}
                spellCheck={false}
              />
            </label>
            <div className="checks">
              <label>
                <input
                  type="checkbox"
                  checked={extended}
                  onChange={(e) => setExtended(e.target.checked)}
                />
                {text("main:29_bit_id")}
              </label>
              <label>
                <input
                  type="checkbox"
                  disabled={kind !== "fd_data"}
                  checked={brs}
                  onChange={(e) => setBrs(e.target.checked)}
                />
                BRS
              </label>
            </div>
            {kind === "classic_remote" ? (
              <label>
                {text("main:requested_dlc")}
                <input
                  type="number"
                  min="0"
                  max="8"
                  value={dlc}
                  onChange={(e) => setDlc(Number(e.target.value))}
                />
              </label>
            ) : (
              <label>
                {text("main:payload")}
                <small>{text("main:hex_bytes")}</small>
                <textarea
                  value={data}
                  onChange={(e) => setData(e.target.value)}
                  rows={4}
                  spellCheck={false}
                />
              </label>
            )}
            <p>
              {kind === "classic_remote"
                ? 0
                : Math.floor(data.replace(/\s/g, "").length / 2)}{" "}
              {text("main:payload_bytes")}
            </p>
            {frameError && <p role="alert">{frameError}</p>}
            {sendBlocked && sendBlocked !== frameError && <p>{sendBlocked}</p>}
            <TransmissionActions
              auth={auth}
              channel={session?.channel_id}
              session={selectedId}
              generation={lease?.generation}
              disabled={!!sendBlocked}
              frame={() => validateFrame(kind, id, extended, data, brs, dlc)}
              onError={reportError}
              onNotice={setNotice}
            />
            <TaskPanel
              key={auth?.client_id ?? "unpaired"}
              auth={auth}
              session={selectedId}
              generation={lease?.generation}
              disabled={!!sendBlocked}
              frame={() => validateFrame(kind, id, extended, data, brs, dlc)}
              onError={reportError}
              onControlLost={() => setLease(null)}
            />
            <p className="transmit-note">
              {text(
                "main:requires_a_running_active_channel_and_control_submission_does",
              )}
            </p>
          </section>
        </div>
        <footer id="about">
          <strong>CANHarbor</strong>
          <span>AGPL-3.0-or-later</span>
          <a
            href="https://github.com/soulteary/canharbor"
            target="_blank"
            rel="noreferrer"
          >
            {text("main:source_repository")}
          </a>
          <a href="/api/v1/legal" target="_blank" rel="noreferrer">
            {text("main:build_license")}
          </a>
          <span className="footer-end">
            {text("main:development_build_hardware_compatibility_unverified")}
          </span>
        </footer>
      </main>
    </div>
  );
}
void initializeLanguage().then(() => {
  createRoot(document.getElementById("root")!).render(<App />);
});
