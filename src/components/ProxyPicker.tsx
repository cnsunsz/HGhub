import { useEffect, useState } from "react";
import { api, isMobile, type ProxyState } from "../api";
import { useStore } from "../store";
import { Modal, Sheet } from "./ui";
import { ICheck, IClose, ILink, IRefresh } from "../icons";

type Mode = ProxyState["mode"];

function ModeRow({ mode, current, title, desc, disabled, onPick }: {
  mode: Mode; current: boolean; title: string; desc: string; disabled: boolean; onPick: (m: Mode) => void;
}) {
  return (
    <button onClick={() => onPick(mode)} disabled={disabled}
      className={`w-full flex items-start gap-2 px-3.5 py-2.5 text-left pressable disabled:opacity-60 ${current ? "bg-s3/70" : ""}`}>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 text-[12.5px] text-on">
          <span>{title}</span>
          {current && <ICheck size={13} className="text-mint shrink-0" />}
        </div>
        <div className="text-[10.5px] text-mut mt-0.5">{desc}</div>
      </div>
    </button>
  );
}

export default function ProxyPicker({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { toast } = useStore();
  const [state, setState] = useState<ProxyState | null>(null);
  const [manual, setManual] = useState("");
  const [saving, setSaving] = useState(false);

  const readState = () => {
    api.proxyState()
      .then((s) => {
        setState(s);
        setManual(s.url || "http://127.0.0.1:1082");
      })
      .catch((e) => toast((e as Error).message || "读取代理设置失败", "info"));
  };

  useEffect(() => {
    if (!open) return;
    readState();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const save = async (mode: Mode, url?: string) => {
    setSaving(true);
    try {
      const next = await api.proxySet(mode, url);
      setState(next);
      setManual(next.url || url || manual);
      toast(mode === "off" ? "已关闭本地代理" : mode === "auto" ? "已跟随系统网络（含小火箭 VPN）" : next.effective ? `已使用 ${next.effective}` : "已保存", "ok");
    } catch (e) {
      toast((e as Error).message || "保存代理设置失败", "info");
    } finally {
      setSaving(false);
    }
  };

  const body = (
    <div className="flex flex-col min-h-0" style={{ maxHeight: isMobile ? "74vh" : "min(72vh, 520px)" }}>
      <div className="shrink-0 flex items-center gap-2 px-4 pt-3 pb-2">
        <span className="w-8 h-8 rounded-full bg-s3 flex items-center justify-center text-primary"><ILink size={16} /></span>
        <div className="flex-1 min-w-0">
          <div className="text-[14px] font-bold text-on leading-tight">本地代理</div>
          <div className="text-[10.5px] text-mut mt-0.5 truncate">
            {state?.effective ? `当前：${state.effective}` : "当前：未使用"}
          </div>
        </div>
        <button onClick={onClose} aria-label="关闭" className="w-8 h-8 rounded-full bg-s3/80 flex items-center justify-center text-mut pressable"><IClose size={16} /></button>
      </div>

      <div className="flex-1 scroll-y px-3 pb-4">
        <div className="rounded-(--r-md) bg-s2 divide-y divide-s3 overflow-hidden">
          <ModeRow mode="auto" current={state?.mode === "auto"} disabled={saving}
            title="跟随系统" desc="走系统网络和小火箭 VPN，不强制使用 1082 本地端口"
            onPick={(m) => { void save(m, state?.url); }} />
          <ModeRow mode="off" current={state?.mode === "off"} disabled={saving}
            title="关闭" desc="不走本机 HTTP/SOCKS 代理，仅保留直连或系统 TUN"
            onPick={(m) => { void save(m, state?.url); }} />
          <ModeRow mode="manual" current={state?.mode === "manual"} disabled={saving}
            title="手动填写" desc="适用于自定义端口，例如 http://127.0.0.1:1082"
            onPick={(m) => { if (m === "manual") setState((s) => s ? { ...s, mode: "manual" } : s); }} />
        </div>

        {(state?.mode === "manual") && (
          <div className="mt-3 px-1">
            <div className="text-[10.5px] font-semibold text-dim tracking-wide mb-1.5">代理地址</div>
            <input
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              placeholder="http://127.0.0.1:1082"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              className="w-full h-9 px-3 rounded-(--r-sm) bg-s2 text-[12.5px] text-on outline-none"
            />
            <div className="flex items-center gap-2 mt-2">
              {state?.mode === "manual" && (
                <button disabled={saving || !manual.trim()} onClick={() => { void save("manual", manual.trim()); }}
                  className="h-8 px-3 rounded-(--r-sm) bg-primary text-[12px] text-on-primary pressable disabled:opacity-50">
                  保存并使用
                </button>
              )}
              {state?.mode === "auto" && (
                <button disabled={saving} onClick={() => { void save("auto", manual.trim()); }}
                  className="h-8 px-3 rounded-(--r-sm) bg-s4 text-[12px] text-on pressable disabled:opacity-50 inline-flex items-center gap-1">
                  <IRefresh size={12} />重新检测
                </button>
              )}
            </div>
          </div>
        )}

        <div className="text-[10px] text-dim leading-relaxed px-1 mt-4">
          iOS 版网络请求不走浏览器代理。使用 Shadowrocket 时请先打开小火箭，再选「自动检测」；若改过端口，请手动填写 HTTP 地址。
        </div>
      </div>
    </div>
  );

  return isMobile
    ? <Sheet open={open} onClose={onClose}>{body}</Sheet>
    : <Modal open={open} onClose={onClose} width={420}>{body}</Modal>;
}
