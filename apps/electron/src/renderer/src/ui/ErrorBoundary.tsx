// Last line of defence: a rendering error shows a recoverable message instead of a blank window.

import { Component, type ErrorInfo, type ReactNode } from "react";

import { t } from "@/lib/i18n";

interface State {
  error: Error | null;
}

export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("UI error", error, info.componentStack);
  }

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="fatal" role="alert">
        <h2>{t("界面出错了")}</h2>
        <p className="mono">{error.message}</p>
        <div className="fatal-actions">
          <button className="ghost" onClick={() => this.setState({ error: null })}>
            {t("重试")}
          </button>
          <button className="primary" onClick={() => location.reload()}>
            {t("重新加载界面")}
          </button>
        </div>
      </div>
    );
  }
}
