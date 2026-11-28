import { useEffect, useState } from "react";
import {
  api,
  IpInfo,
  listenSpeedTestProgress,
  NetworkIpReport,
  SpeedProgress,
  SpeedTestResult,
} from "../api";
import { fmtBytes, fmtSpeed } from "../App";
import { t } from "../i18n";
import { showToast } from "../toast";

function fmtMbps(bps: number): string {
  return `${((bps * 8) / 1_000_000).toFixed(1)} Mbps`;
}

function fmtLocation(info: IpInfo): string {
  const parts = [info.country, info.region, info.city].filter(Boolean);
  const uniq: string[] = [];
  for (const p of parts) {
    if (p && uniq[uniq.length - 1] !== p) uniq.push(p);
  }
  return uniq.join(" · ") || "—";
}

function IpTile({
  label,
  info,
  error,
  loading,
}: {
  label: string;
  info: IpInfo | null;
  error?: string | null;
  loading: boolean;
}) {
  const copyIp = async () => {
    if (!info?.ip) return;
    try {
      await navigator.clipboard.writeText(info.ip);
      showToast(t("copied"), "info");
    } catch {
      // ignore clipboard errors
    }
  };
  return (
    <div className="ip-box">
      <div className="ip-box-head">
        <span className="ip-box-label">{label}</span>
        {info && (
          <span className="ip-source-tag">
            {info.source} · {info.latency_ms} ms
          </span>
        )}
      </div>
      {loading && !info ? (
        <div className="hint" style={{ marginTop: 8 }}>{t("netDiagIpDetecting")}</div>
      ) : info ? (
        <>
          <div className="ip-value-row">
            <code className="ip-value">{info.ip}</code>
            <button type="button" className="text-button" onClick={() => void copyIp()}>
              {t("copyLink")}
            </button>
          </div>
          <div className="ip-meta">
            <div>
              <span>{t("netDiagLocation")}:</span>
              <strong>{fmtLocation(info)}</strong>
            </div>
            <div>
              <span>{t("netDiagIsp")}:</span>
              <strong>
                {[info.asn, info.isp].filter(Boolean).join(" · ") || "—"}
              </strong>
            </div>
          </div>
        </>
      ) : (
        <div className="error" style={{ marginTop: 6 }}>
          {error || t("netDiagIpFailed")}
        </div>
      )}
    </div>
  );
}

export function NetDiagPage({
  onOpenProxySettings,
}: {
  onOpenProxySettings: () => void;
}) {
  const [ipReport, setIpReport] = useState<NetworkIpReport | null>(null);
  const [ipLoading, setIpLoading] = useState(true);

  const [server, setServer] = useState<"auto" | "cloudflare" | "cachefly">("auto");
  const [testing, setTesting] = useState(false);
  const [live, setLive] = useState<SpeedProgress | null>(null);
  const [samples, setSamples] = useState<number[]>([]);
  const [result, setResult] = useState<SpeedTestResult | null>(null);
  const [speedErr, setSpeedErr] = useState<string | null>(null);

  const refreshIp = async () => {
    setIpLoading(true);
    try {
      setIpReport(await api.detectNetworkIp());
    } catch (e) {
      setIpReport({
        proxy_mode: "system",
        effective_proxy: null,
        active: null,
        active_error: String(e),
        direct: null,
      });
    } finally {
      setIpLoading(false);
    }
  };

  useEffect(() => {
    void refreshIp();
    let un: (() => void) | undefined;
    let alive = true;
    listenSpeedTestProgress((p) => {
      if (!alive) return;
      setLive(p);
      if (p.phase === "download" && p.current_bps > 0) {
        setSamples((prev) => [...prev.slice(-39), p.current_bps]);
      }
    }).then((fn) => {
      if (alive) un = fn;
      else fn();
    });
    return () => {
      alive = false;
      un?.();
      api.cancelSpeedTest().catch(() => {});
    };
  }, []);

  const startSpeed = async () => {
    if (testing) return;
    setTesting(true);
    setSpeedErr(null);
    setResult(null);
    setLive(null);
    setSamples([]);
    try {
      const r = await api.runSpeedTest(server);
      setResult(r);
    } catch (e) {
      setSpeedErr(String(e));
    } finally {
      setTesting(false);
    }
  };

  const stopSpeed = async () => {
    try {
      await api.cancelSpeedTest();
    } catch {
      // ignore cancel errors
    }
  };

  const routeLabel =
    ipReport?.proxy_mode === "direct"
      ? t("proxyModeDirect")
      : ipReport?.proxy_mode === "custom"
        ? t("proxyModeCustom")
        : t("proxyModeSystem");

  const avgBps = testing ? (live?.avg_bps ?? 0) : (result?.avg_bps ?? 0);
  const peakBps = testing ? (live?.peak_bps ?? 0) : (result?.peak_bps ?? 0);
  const latencyMs = testing ? (live?.latency_ms ?? null) : (result?.latency_ms ?? null);
  const jitterMs = testing ? (live?.jitter_ms ?? null) : (result?.jitter_ms ?? null);
  const totalBytes = testing ? (live?.bytes ?? 0) : (result?.bytes ?? 0);
  const serverName = testing ? live?.server : result?.server;
  const elapsedPct = testing
    ? live?.phase === "ping"
      ? 12
      : Math.min(98, 12 + Math.round(((live?.elapsed_ms ?? 0) / 8000) * 86))
    : result && !result.canceled
      ? 100
      : 0;

  const chartVals = samples.length > 0 ? samples : [0, 0];
  const chartMax = Math.max(...chartVals, 1024 * 64);
  const pts = chartVals
    .map((v, i) => {
      const x = chartVals.length <= 1 ? 0 : (i / (chartVals.length - 1)) * 320;
      const y = 66 - (v / chartMax) * 58;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");

  return (
    <section className="settings-page netdiag-page" aria-label={t("netDiagTitle")}>
      <div className="settings-page-body netdiag-body">
        <div className="settings-cards">
          <div className="netdiag-route-bar">
            <div className="netdiag-route-info">
              <span className="netdiag-route-label">{t("netDiagProxyRoute")}:</span>
              <strong>{routeLabel}</strong>
              {ipReport?.effective_proxy && <code>{ipReport.effective_proxy}</code>}
            </div>
            <button type="button" className="text-button" onClick={onOpenProxySettings}>
              {t("netDiagOpenProxySettings")}
            </button>
          </div>

          <div className="settings-card">
            <div className="netdiag-card-head">
              <div className="settings-card-title" style={{ marginBottom: 0 }}>
                {t("netDiagIpCard")}
              </div>
              <button
                type="button"
                disabled={ipLoading}
                onClick={() => void refreshIp()}
              >
                {ipLoading ? t("netDiagIpDetecting") : t("netDiagIpRefresh")}
              </button>
            </div>
            <div className={`ip-grid${ipReport?.direct ? " two" : ""}`}>
              <IpTile
                label={t("netDiagIpActive")}
                info={ipReport?.active ?? null}
                error={ipReport?.active_error}
                loading={ipLoading}
              />
              {ipReport?.direct && (
                <IpTile
                  label={t("netDiagIpDirect")}
                  info={ipReport.direct}
                  loading={ipLoading}
                />
              )}
            </div>
          </div>

          <div className="settings-card">
            <div className="netdiag-card-head">
              <div className="settings-card-title" style={{ marginBottom: 0 }}>
                {t("netDiagSpeedCard")}
              </div>
            </div>
            <div className="settings-field-row">
              <div style={{ flex: 1, minWidth: 0 }}>
                <select
                  aria-label={t("netDiagSpeedCard")}
                  value={server}
                  disabled={testing}
                  onChange={(e) =>
                    setServer(e.target.value as "auto" | "cloudflare" | "cachefly")
                  }
                >
                  <option value="auto">{t("netDiagServerAuto")}</option>
                  <option value="cloudflare">{t("netDiagServerCloudflare")}</option>
                  <option value="cachefly">{t("netDiagServerCachefly")}</option>
                </select>
              </div>
              {testing ? (
                <button type="button" className="danger" onClick={() => void stopSpeed()}>
                  {t("netDiagStopSpeed")}
                </button>
              ) : (
                <button type="button" className="primary" onClick={() => void startSpeed()}>
                  {t("netDiagStartSpeed")}
                </button>
              )}
            </div>

            {testing && (
              <div className="hint" style={{ marginTop: 10 }}>
                {live?.phase === "ping" ? t("netDiagTestingPing") : t("netDiagTestingDl")}
                {serverName ? ` · ${serverName}` : ""}
              </div>
            )}
            {!testing && result && (
              <div className="hint" style={{ marginTop: 10 }}>
                {serverName} · {t("netDiagTransferred")}: {fmtBytes(totalBytes)}
              </div>
            )}

            {(testing || result) && (
              <div className="bar" style={{ marginTop: 8 }}>
                <div className="fill" style={{ width: `${elapsedPct}%` }} />
              </div>
            )}

            <div className="speed-metrics">
              <div className="metric-tile">
                <span className="metric-label">{t("netDiagAvgSpeed")}</span>
                <strong className="metric-val">{avgBps > 0 ? fmtSpeed(avgBps) : "—"}</strong>
                <span className="metric-sub">{avgBps > 0 ? fmtMbps(avgBps) : "—"}</span>
              </div>
              <div className="metric-tile">
                <span className="metric-label">{t("netDiagPeakSpeed")}</span>
                <strong className="metric-val">{peakBps > 0 ? fmtSpeed(peakBps) : "—"}</strong>
                <span className="metric-sub">{peakBps > 0 ? fmtMbps(peakBps) : "—"}</span>
              </div>
              <div className="metric-tile">
                <span className="metric-label">{t("netDiagLatency")}</span>
                <strong className="metric-val">
                  {latencyMs != null ? `${latencyMs} ms` : "—"}
                </strong>
                <span className="metric-sub">
                  {jitterMs != null ? `${t("netDiagJitter")}: ${jitterMs} ms` : "—"}
                </span>
              </div>
            </div>

            {samples.length > 1 && (
              <div className="speed-trend-box">
                <svg
                  className="speed-trend-svg"
                  viewBox="0 0 320 70"
                  preserveAspectRatio="none"
                  aria-label={t("netDiagAvgSpeed")}
                >
                  <path
                    d={`M0 68 L${pts} L320 68 Z`}
                    fill="var(--accent-soft)"
                  />
                  <polyline
                    points={pts}
                    fill="none"
                    stroke="var(--accent)"
                    strokeWidth="2"
                    strokeLinejoin="round"
                  />
                </svg>
              </div>
            )}

            {speedErr && <div className="error">{speedErr}</div>}
          </div>
        </div>
      </div>
    </section>
  );
}
