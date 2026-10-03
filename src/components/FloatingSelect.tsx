import { useId, useState, type CSSProperties } from "react";
import { Select, theme } from "antd";
import "./FloatingSelect.css";

type FloatingSelectProps = {
  label: string;
  value?: string;
  onChange: (value: string) => void;
  onSelect?: (value: string) => void;
  options: { value: string; label: string; disabled?: boolean }[];
  disabled?: boolean;
  size?: "small" | "middle" | "large";
};

export function FloatingSelect({ label, value, onChange, onSelect, options, disabled, size }: FloatingSelectProps) {
  const id = useId();
  const { token } = theme.useToken();
  const [focused, setFocused] = useState(false);
  const [open, setOpen] = useState(false);
  const floating = focused || open || (value !== undefined && value !== "");
  const style = {
    "--select-radius": `${token.borderRadius}px`, "--select-font": `${token.fontSize}px`,
    "--select-family": token.fontFamily, "--select-hover": token.colorPrimaryHover,
    "--select-disabled": token.colorBgContainerDisabled, "--select-text-disabled": token.colorTextDisabled,
    "--select-border": token.colorBorder,
    "--select-primary": token.colorPrimary,
    "--select-bg": token.colorBgContainer,
    "--select-label": token.colorTextDescription,
  } as CSSProperties;
  return (
    <div className="floating-select" style={style} data-floating={floating} data-focused={focused || open} data-disabled={disabled}>
      <Select id={id} aria-labelledby={`${id}-label`} value={value} onChange={onChange} onSelect={onSelect} options={options} disabled={disabled} size={size}
        variant="borderless" onFocus={() => setFocused(true)} onBlur={() => setFocused(false)} onOpenChange={setOpen}
        style={{ width: "100%", height: size === "small" ? token.controlHeightSM : size === "large" ? token.controlHeightLG : token.controlHeight }}
        suffixIcon={<svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" style={{transform:open ? "rotate(180deg)" : undefined}}><path d="m7 10 5 5 5-5z" /></svg>} />
      <label id={`${id}-label`} htmlFor={id}>{label}</label>
      <span className="floating-select-outline" aria-hidden="true" />
    </div>
  );
}


