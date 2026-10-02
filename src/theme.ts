import type { ThemeConfig } from "antd";

// Shared sizes: small 32px, default 40px, large 48px.
export const appTheme: ThemeConfig = {
  token: {
    colorPrimary: "#1677ff",
    borderRadius: 6,
    borderRadiusSM: 6,
    borderRadiusLG: 6,
    controlHeight: 40,
    controlHeightSM: 32,
    controlHeightLG: 48,
    fontSize: 14,
    fontSizeLG: 14,
    fontFamily: '"Segoe UI", "Microsoft YaHei", sans-serif',
  },
  components: {
    Input: { activeShadow: "none", errorActiveShadow: "none", warningActiveShadow: "none" },
    Select: { activeOutlineColor: "transparent", optionHeight: 40, optionPadding: "8px 12px", optionSelectedFontWeight: 500 },
    Tabs: { horizontalItemGutter: 0 },
    Table: { bodySortBg: "transparent", headerBg: "#fafafa", headerSortActiveBg: "#fafafa" },
  },
};
