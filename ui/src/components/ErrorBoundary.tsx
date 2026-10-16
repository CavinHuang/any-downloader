import React from "react";
import { t } from "../i18n";

/** 渲染错误兜底：任何子树崩溃显示错误面板而非白/黑屏。 */
export class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("render crash:", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 32, color: "var(--err-ink)", userSelect: "text" }}>
          <h3>{t("crashTitle")}</h3>
          <pre style={{ whiteSpace: "pre-wrap", fontSize: 12 }}>
            {String(this.state.error.stack || this.state.error)}
          </pre>
          <button onClick={() => this.setState({ error: null })}>{t("retry")}</button>
        </div>
      );
    }
    return this.props.children;
  }
}

