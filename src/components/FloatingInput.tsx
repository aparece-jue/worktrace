import { forwardRef, useId, useState, type CSSProperties, type ReactNode } from "react";
import { Form, Input, theme, type InputProps, type InputRef } from "antd";
import "./FloatingInput.css";

export type FloatingInputProps = Omit<InputProps, "prefix" | "suffix" | "addonBefore" | "addonAfter" | "variant"> & {
  label: string;
  helperText?: ReactNode;
};
// 
/** Ant Design Input with an outlined floating label. Supports Form.Item value/onChange/ref. */
export const FloatingInput = forwardRef<InputRef, FloatingInputProps>(function FloatingInput(
  { label, helperText, id, value, defaultValue, onChange, onFocus, onBlur, status, disabled,
    className, style, placeholder, required, size, "aria-describedby": describedBy, ...props }, ref,
) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const { token } = theme.useToken();
  const effectiveStatus = status;
  const [focused, setFocused] = useState(false);
  const [localValue, setLocalValue] = useState(defaultValue ?? "");
  const filled = String(value !== undefined ? value ?? "" : localValue).length > 0;
  const floating = focused || filled;
  const error = effectiveStatus === "error";
  const accent = error ? token.colorError : effectiveStatus === "warning" ? token.colorWarning : token.colorPrimary;
  const variables = {
    "--float-height": `${size === "small" ? token.controlHeightSM : size === "large" ? token.controlHeightLG : token.controlHeight}px`,
    "--float-font": `${token.fontSize}px`, "--float-family": token.fontFamily, "--float-hover": token.colorPrimaryHover,
    "--float-accent": accent, "--float-border": error || effectiveStatus === "warning" ? accent : token.colorBorder,
    "--float-label": token.colorTextDescription, "--float-bg": token.colorBgContainer,
    "--float-disabled": token.colorBgContainerDisabled, "--float-radius": `${token.borderRadius}px`, ...style,
  } as CSSProperties;

  return (
    <div className={`floating-input ${className ?? ""}`} style={variables} data-floating={floating} data-focused={focused} data-disabled={disabled} data-size={size} data-error={error}>
      <div className="floating-input-control">
        <Input {...props} ref={ref} id={inputId} value={value} defaultValue={defaultValue} disabled={disabled}
          required={required} size={size} status={status} variant="borderless"
          placeholder={floating ? placeholder : undefined} aria-invalid={error || undefined}
          aria-describedby={[describedBy, helperText ? `${inputId}-help` : undefined].filter(Boolean).join(" ") || undefined}
          onChange={(event) => { setLocalValue(event.target.value); onChange?.(event); }}
          onFocus={(event) => { setFocused(true); onFocus?.(event); }}
          onBlur={(event) => { setFocused(false); onBlur?.(event); }} />
        <label htmlFor={inputId}>{label}{required ? " *" : ""}</label>
        <span className="floating-input-outline" aria-hidden="true" />
      </div>
      {helperText && <div className="floating-input-help" id={`${inputId}-help`}>{helperText}</div>}
    </div>
  );
});

/** Use this adapter inside Form.Item to inherit validation status. */
export const FormFloatingInput = forwardRef<InputRef, FloatingInputProps>(function FormFloatingInput(props, ref) {
  const { status } = Form.Item.useStatus();
  return <FloatingInput {...props} ref={ref} status={props.status ?? (status === "error" || status === "warning" ? status : undefined)} />;
});


