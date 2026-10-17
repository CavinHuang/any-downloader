import { useEffect, useState } from "react";
import { api } from "../api";
import { t } from "../i18n";
import { useOverlay } from "../useOverlay";
import { Icon } from "./Icon";

interface BundleInfo {
  path: string | null;
  present: boolean;
}

/** 「安装浏览器扩展」弹窗：引导把随桌面端安装的扩展加载进 Chrome / Edge。 */
export function ExtensionDialog({ onClose }: { onClose: () => void }) {
  const [info, setInfo] = useState<BundleInfo | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { ref, onKeyDown } = useOverlay(onClose);

  useEffect(() => {
    api.extensionBundle().then(setInfo).catch((e) => setError(String(e)));
  }, []);

  const copyPath = async () => {
    if (!info?.path) return;
    try {
      await navigator.clipboard.writeText(info.path);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const openFolder = async () => {
    if (!info?.path) return;
    try {
      await api.openExtensionDir(info.path);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="modal-mask" onClick={onClose}>
      <div
        className="modal extension-modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("extInstallTitle")}
        ref={ref}
        onKeyDown={onKeyDown}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="dialog-header">
          <div>
            <div className="eyebrow">COLLECT AS YOU BROWSE</div>
            <h3>{t("extInstallTitle")}</h3>
          </div>
          <button className="icon-button" onClick={onClose} aria-label={t("close")}>
            <Icon name="close" />
          </button>
        </div>
        <p className="dialog-description">{t("extInstallIntro")}</p>
        <ol className="ext-steps">
          <li>{t("extStep1")}</li>
          <li>{t("extStep2")}</li>
          <li>{t("extStep3")}</li>
          <li>{t("extStep4")}</li>
        </ol>
        <div className="ext-path">
          <span className="ext-path-label">{t("extDirLabel")}</span>
          {info == null ? (
            <span style={{ color: "var(--muted)" }}>…</span>
          ) : info.present && info.path ? (
            <code className="ext-path-value">{info.path}</code>
          ) : (
            <span className="ext-path-missing">{t("extDirMissing")}</span>
          )}
        </div>
        {info?.present && info.path && (
          <div style={{ display: "flex", gap: 8 }}>
            <button onClick={openFolder}>{t("extOpenFolder")}</button>
            <button onClick={copyPath}>
              {copied ? t("extCopied") : t("extCopyPath")}
            </button>
          </div>
        )}
        {error && <div className="error">{error}</div>}
        <div className="modal-actions">
          <button
            className="text-button"
            onClick={() => api.openExtensionDoc().catch((e) => setError(String(e)))}
          >
            {t("extDocLink")}
          </button>
          <div className="spacer" />
          <button onClick={onClose}>{t("close")}</button>
        </div>
      </div>
    </div>
  );
}
