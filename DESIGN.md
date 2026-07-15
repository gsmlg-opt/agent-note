---
version: '0.2'
name: 'ZDNS UI Design System'
description: 'Portable visual design contract for ZDNS Web apps using @zdns/design and @zddi/components — aligned with Figma ZDNS-UI「Color Light Mode」.'

maintenance: 'manual-with-validation'
external_packages:
  - '@zdns/design@2.0.0'
  - '@zddi/components@5.0.0'
figma:
  file: 'https://www.figma.com/design/kmgk5sM13HYurx3jUfe8Cw/ZDNS-UI设计'
  color_light_mode: 'node 7390:73953 Color Light Mode - 明亮'

colors:
  primary: '#0065ff'
  on-primary: '#ffffff'
  primary-hover: '#619fff'
  surface: '#ffffff'
  surface-container: '#f5f6fa'
  surface-hover: '#eef2f5'
  surface-subtle: '#f3f6fb'
  on-surface: '#5a607f'
  on-surface-body: '#2f2e3f'
  on-surface-strong: '#191919'
  muted: '#a1a7c4'
  outline: '#d7dbec'
  divider: '#e6e9f4'
  link: '#0065ff'
  info: '#0065ff'
  info-container: '#ddeaff'
  success: '#52c41a'
  success-container: '#e5f6dd'
  on-success: '#006633'
  warning: '#faad14'
  warning-container: '#fef3dc'
  on-warning: '#663d00'
  error: '#ff4444'
  error-container: '#ffe9e9'
  on-error: '#ffffff'
  selection: '#ddeaff'
  empty-description: '#a1a7c4'
  error-description: '#ff4444'

typography:
  body: "PingFang SC, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif (Figma spec; runtime still inherits @zdns/design until ConfigProvider fontFamily is set)"
  heading: 'PingFang SC Medium for section titles in Figma; weight 500–600 in examples'
  mono: 'not configured — use @zdns/design defaults if needed'
  sizes:
    tooltip: '12px'
    table-small: '13px'
    tab: '15px'
    body: '14px / 24px line-height (Figma 大正文)'
    table-header-accent: '16px'

rounded:
  none: '0'
  sm: '4px'
  md: '6px'
  field: '0.5rem'
  box: '2rem'
  pill: '999px'

spacing:
  xs: '4px'
  sm: '8px'
  md: '16px'
  lg: '20px'
  xl: '32px'
  layout-content-min-width: '640px'
  table-pagination-margin-top: '16px'

elevation:
  modal-z-index: '1001'
  loading-backdrop-z-index: '99'
  dropdown-popup-z-index: '10000000'
  card-shadow: '0 3px 6px rgba(47, 46, 63, 0.05) (often commented out; @zdns/design Card defaults apply)'

border:
  default: '0.5px (DaisyUI --border when used)'

breakpoints:
  note: 'No custom Tailwind breakpoints required — use @zdns/design responsive behavior and layout min-widths'
  layout-content-min: '640px'
---

# Overview

ZDNS 企业级管理后台的视觉语言：色板对齐 Figma **Color Light Mode（明亮）**，品牌蓝 `#0065FF` 为主色，浅色表面、清晰表格与侧栏布局。组件行为由 **`@zdns/design`**（Ant Design 体系）与 **`@zddi/components`**（如 ZModal、GridEditTable）提供；各项目可再通过 CSS 变量 / 主题注入做品牌对齐。

**权威来源（按优先级）：**

1. **Figma UI 规范** — [ZDNS-UI设计 · Color Light Mode](https://www.figma.com/design/kmgk5sM13HYurx3jUfe8Cw/ZDNS-UI设计?node-id=7390-73953)
2. **运行时主题** — 应用内 `ConfigProvider` 的 `theme.token`，以及 `setZDesignPrimaryColor` / `setModalFromPrimaryColor`
3. **语义 CSS 变量** — `--color-primary`、`--z-design-*` 等注入 `@zdns/design` 子系统
4. **外部包默认** — `@zdns/design`、`@zddi/components` 未覆盖样式

本文件（`DESIGN.shared.md`）是**可跨项目共享**的品牌契约：人工维护；勿写入具体仓库路径。消费方项目用 `DESIGN.project.md`（可选）记录本地落地路径，再合成根目录 `DESIGN.md` 供 agent / 校验使用。

## 同栈项目如何使用

适用于同组织、同样依赖 `@zdns/design` + `@zddi/components` 的 React/Web 后台：

1. **短期**：拷贝本文件到消费方根目录，可直接作为 `DESIGN.md`，或保留为 `DESIGN.shared.md` 并自建 `DESIGN.project.md` 后合成。
2. **中期（推荐）**：随 `@zdns/design`（或独立 `@zdns/design-docs`）发版；消费方跟包升版本同步本文件。
3. UI 改动时：**改品牌**改本文件（及 Figma）；**改本仓落地**改源码 + `DESIGN.project.md`，勿只改合成产物。

---

# Brand and Visual Direction

- **产品气质**：B2B 网络/DNS 管理控制台 — 信息密度适中、表格与表单为主、侧栏导航。
- **主色**：ZDNS 蓝 `#0065FF`（Figma 主题色），用于主按钮、链接、选中态、图标强调。
- **背景**：白底内容区 `#FFFFFF` + 选项卡/分区 `#F5F6FA`；页面浅背景 `#F3F6FB`；列表悬浮 `#EEF2F5`。
- **组件来源**：通过项目内统一封装层消费 `@zdns/design` / `@zddi/components`，避免业务页散落直连（便于主题注入）。
- **图表**：ECharts 系列色对齐 Figma 数据可视化色前 10 项；完整 27 色见下文。

---

# Colors

## Semantic tokens

| Token                 | Value     | 用途（Figma）                         | 可用                                                    | 禁止                                |
| --------------------- | --------- | ------------------------------------- | ------------------------------------------------------- | ----------------------------------- |
| `primary`             | `#0065FF` | 主题色                                | Button `type="primary"`、链接、`setZDesignPrimaryColor` | 大面积背景、正文文字                |
| `on-primary`          | `#FFFFFF` | 主色上的文字/图标                     | 实心主按钮文字                                          | 浅色背景正文                        |
| `primary-hover`       | `#619FFF` | 链接/主色 hover（卡片渐变终点）       | 链接 hover、`--color-primary-hover`                     | 作为默认链接色                      |
| `surface`             | `#FFFFFF` | 页面白背景                            | 页面容器、Card body                                     | —                                   |
| `surface-container`   | `#F5F6FA` | 选项卡背景、次级表面                  | 次级表面 / DaisyUI `base-200`（若使用）                   | 主文本区域若对比不足                |
| `surface-hover`       | `#EEF2F5` | 列表悬浮、Table 相关背景              | 列表行 hover                                            | 正文文字                            |
| `surface-subtle`      | `#F3F6FB` | 页面浅背景                            | 页面级浅底                                              | —                                   |
| `on-surface-strong`   | `#191919` | 一级标题（变量）；色板标注为黑 88%    | 大标题、强对比标题                                      | 弱对比辅助文案                      |
| `on-surface-body`     | `#2F2E3F` | 次标题、常规字色                      | 正文、次级标题                                          | placeholder                         |
| `on-surface`          | `#5A607F` | 辅助字色                              | 表格浅色表头文字、Tab 默认色                            | 需要 AAA 对比的小字号正文（需验证） |
| `muted`               | `#A1A7C4` | placeholder、说明字色                 | 空态描述                                                | 可点击控件的唯一颜色                |
| `outline`             | `#D7DBEC` | 细线 border                           | 次级边框                                                | 错误/警告边框                       |
| `divider`             | `#E6E9F4` | 分割线                                | `--color-divider`                                       | 可点击边框唯一色                    |
| `success`             | `#52C41A` | 成功提示色                            | 语义成功、状态图标                                      | 普通链接                            |
| `success-container`   | `#E5F6DD` | 成功背景色                            | Alert/Tag 浅绿底                                        | 正文                                |
| `warning`             | `#FAAD14` | 告警提示色                            | 语义告警                                                | 成功/错误混用                       |
| `warning-container`   | `#FEF3DC` | 告警背景色                            | Alert/Tag 浅黄底                                        | 正文                                |
| `error`               | `#FF4444` | 危险提示色（全链路统一）              | `colorError`、danger 按钮                               | 旧色 `#F5493A` / `#FF4D4F`          |
| `error-container`     | `#FFE9E9` | 危险背景色                            | Alert/Tag 浅红底                                        | 正文                                |
| `on-error`            | `#FFFFFF` | 危险实心按钮上的文字                  | danger 实心按钮                                         | 浅红底上的正文（改用 error）        |
| `selection`           | `#DDEAFF` | 树节点选中 / 与提示背景同色           | 树选中态                                                | 全局选中文本                        |
| `info` / `link`       | `#0065FF` | 提示色（与主题色一致）                | Alert/Badge 信息类、链接                                | —                                   |
| `info-container`      | `#DDEAFF` | 提示背景色                            | Alert info 底、选中态                                   | —                                   |

## CSS variable aliases

推荐在应用全局样式中定义语义变量，并将主色注入 `@zdns/design` 子系统：

```css
--color-primary: #0065ff;
--color-primary-hover: #619fff;
--color-error: #ff4444;
/* …中性色、功能色背景 */

--z-design-button-color: #0065ff;
--z-design-input-color: #0065ff;
/* …table, menu, dropdown, pagination, search, date-picker, filter */
```

## 数据可视化色（Figma 颜色1–27）

超出 27 色时循环色值并调整透明度。

| #  | Hex       | #  | Hex       | #  | Hex       |
| -- | --------- | -- | --------- | -- | --------- |
| 1  | `#4C86FC` | 10 | `#FF7EBF` | 19 | `#203469` |
| 2  | `#37CCCC` | 11 | `#ACD7FF` | 20 | `#C61818` |
| 3  | `#FF9264` | 12 | `#19FFD7` | 21 | `#BD900C` |
| 4  | `#637196` | 13 | `#19F2FF` | 22 | `#07784F` |
| 5  | `#FF6262` | 14 | `#19BEFF` | 23 | `#1341A0` |
| 6  | `#F6BD16` | 15 | `#1989FF` | 24 | `#5E1D8B` |
| 7  | `#21BF86` | 16 | `#1955FF` | 25 | `#2D679C` |
| 8  | `#2D5ACD` | 17 | `#1920FF` | 26 | `#5A5F6C` |
| 9  | `#BE86E4` | 18 | `#CF4409` | 27 | `#128484` |

ECharts 默认系列色建议使用 **1–10**。

## 渐变（Figma）

| 用途         | From → To                      |
| ------------ | ------------------------------ |
| 登录按钮     | `#0065FF` → `#3EC5FF`          |
| 柱状图       | `#83F4D2` → `#0FD269`          |
| 柱状图       | `#DAF8FF` → `#0769FF`          |
| 柱状图       | `#FFE793` → `#FF7145`          |
| 进度条（DDI）| `#44A4FC` → `#2D5ACD`          |
| 卡片（主）   | `#0065FF` → `#619FFF`          |
| 卡片（成功） | `#52C41A` → `#52C41A` @ 0.7    |
| 卡片（危险） | `#FF4444` → `#FF4444` @ 0.7    |
| 卡片（告警） | `#FAAD14` → `#FAAD14` @ 0.7    |

## 已知不一致

| 项       | 说明                                                                                         |
| -------- | -------------------------------------------------------------------------------------------- |
| 字体族   | Figma 规定 PingFang SC；运行时常继承 `@zdns/design` 系统栈，需在 `ConfigProvider` 显式覆盖才完全对齐 |
| 深色模式 | **仅浅色主题**作为当前契约；未提供完整 dark 调色板                                              |
| 色板标注 | Figma 色卡「大标题」写 `#000000 0.88`；变量「一级标题」为 `#191919` — 采用后者作实色 token     |

---

# Typography

- **设计规范字体**：PingFang SC（Figma Color / 中文样式）。
- **运行时**：未强制时继承 **`@zdns/design` / Ant Design 5** 系统栈。若需像素级对齐，通过 `ConfigProvider theme.token.fontFamily` 设置，而非页面内联。
- **语言**：可见文案应走项目 i18n（如 Lingui）；勿硬编码未提取字符串到可见 UI。
- **常用字号**：

| 场景         | 字号 | 字重   |
| ------------ | ---- | ------ |
| Tooltip 内容 | 12px | normal |
| 小表格       | 13px | normal |
| 页内 Tab     | 15px | 500    |
| 语义表头     | 16px | normal |
| Figma 大正文 | 14px / line-height 24px | normal |

- **表格行高**：建议 `line-height: 19px`（与常见表格 reset 对齐）。

---

# Layout and Spacing

- **内容区最小宽度**：`640px`，保证主内容可横向滚动而非压碎表格。
- **常用间距阶梯**：`4 / 8 / 16 / 20 / 32` px（`xs`–`xl`）。
- **Card 内边距（建议）**：header `16px 16px 6px 16px`，body `16px 20px`。
- **表格分页**：`margin-top: 16px`。
- 树表分栏、顶栏高度等**实现级常量**见各项目 `DESIGN.project.md`。

---

# Shapes and Borders

| Token             | 值             | 用途                        |
| ----------------- | -------------- | --------------------------- |
| `radius-field`    | `0.5rem` (8px) | 输入框、按钮（design 默认） |
| `radius-box`      | `2rem` (32px)  | 大容器                      |
| `sm`              | `4px`          | 小型控件                    |
| `md`              | `6px`          | Logo、次要圆角              |
| `pill`            | `999px`        | 胶囊标签                    |

**默认边框**：约 `0.5px`；细线色对齐 `outline` `#D7DBEC`。

---

# Elevation and Depth

| 层                | z-index / 阴影                      | 说明                           |
| ----------------- | ----------------------------------- | ------------------------------ |
| Modal（ZModal）   | `1001`                              | 覆盖 wrap/mask                 |
| Loading Backdrop  | `99`                                | 半透明白 `#fff5`               |
| Dropdown 弹出层   | `10000000`                          | 高于 ECharts tooltip           |
| Card 阴影         | `0 3px 6px rgba(47,46,63,0.05)`     | 常以 @zdns/design Card 默认为准 |
| Dropdown 浮层背景 | `#ffffffba`                         | 半透明白                       |

---

# Responsive Design

- **Viewport**：`width=device-width, initial-scale=1.0`。
- **响应式**：优先依赖 `@zdns/design` 组件内置行为（如 Toolbar 溢出折叠）；布局 `min-width: 640px`。
- **表格列**：建议 `minWidth: 80`、可调整列宽、允许换行。

---

# Components

以下描述**视觉与交互外观**，非 props API。

## Button（`@zdns/design` Button / Toolbar item）

| 变体      | 外观                                      |
| --------- | ----------------------------------------- |
| `primary` | 品牌蓝实心；toolbar 主操作                |
| `default` | 描边/默认灰底（design 默认）              |
| `link`    | 文字链，主色；danger 链接受 `color-error` |
| `danger`  | 危险操作（删除等）                        |

**状态**：hover/active/focus/disabled 由 `@zdns/design` 提供；disabled 勿仅用颜色区分，需配合 `disabled` 属性。

## Input / Search

- 聚焦环与边框色跟随 `--z-design-input-color` / `primary`。

## Card

- 标准 Card 来自 `@zdns/design`；可收紧 header/body padding，header 无下边框。

## Dialog / Modal（`ZModal` from `@zddi/components`）

- 主色由 `setModalFromPrimaryColor('#0065FF')` 同步。
- Modal body 限制最大高度并允许滚动（如 `max-height: calc(80vh - 100px)`）。

## Navigation

- 侧栏菜单 + 面包屑；选中/展开态主色。

## Table（DatagridPro / ProTable）

- 推荐列表方案；工具栏全宽；分页右下；空数据用 Empty。

## Editable Table（`GridEditTable`，`@zddi/components`）

- Figma：`ZDNS-UI` → **Edit Table**（node `9443:4125`）。
- 需要分页：顶栏操作 + 搜索；勾选列；单元格常显编辑；底部分页与已选计数。
- 无需分页：高度随内容；底部「+ 添加」；可达上限提示。
- 业务层经统一封装引入，勿大量直连 `@zddi/components`。

## Tree

- 选中节点背景 `#DDEAFF`（`--color-selection`）。

## Alerts / Message

- 使用 `message` from `@zdns/design`；语义色与 `success`/`warning`/`error` 一致。

## Menus / Dropdown

- 弹出层半透明白底、极高 z-index；勿与 ECharts tooltip 冲突。

## Tabs

- 默认文字 `#5A607F`，约 15px / 500。

## Tooltip

- 内文约 `12px`。

## Spin / Loading

- 全屏半透明白 backdrop + 居中 Spin；空态/错误态描述色 `#A1A7C4` / `#FF4444`。

## Status icons

| 语义          | 色值      |
| ------------- | --------- |
| 信息/正常强调 | `#0065FF` |
| 成功          | `#52C41A` |
| 警告          | `#FAAD14` |
| 错误          | `#FF4444` |
| 中性/说明     | `#A1A7C4` |

---

# Interaction States

| 状态                | 约定                                                  |
| ------------------- | ----------------------------------------------------- |
| **Default**         | `@zdns/design` 默认 + 语义 CSS 变量                   |
| **Hover**           | 链接 → `#619FFF`；按钮/菜单 follow design             |
| **Active/Selected** | 树 `#DDEAFF`；菜单/表格 follow design primary         |
| **Focus**           | 使用 design 焦点环；勿 `outline: none` 除非有等价替代 |
| **Disabled**        | design disabled 样式；降低对比度 + 不可点击           |
| **Error**           | 表单校验、danger 按钮、文案统一 `#FF4444`             |
| **Loading**         | Spin / 表格 loading                                   |

---

# Accessibility

- **国际化**：用户可见字符串经 i18n；支持至少 `zh-CN` / `en-US` 时勿硬编码。
- **对比度**：主色 on 白底用于链接/按钮；正文优先 `on-surface-body` `#2F2E3F`；辅助文 `#5A607F` on `#FFFFFF` 需在关键合规场景验证 WCAG。
- **键盘**：优先 design 组件内置支持；自定义交互保留 focus 与 Enter/Escape。
- **状态不仅靠颜色**：状态应同时展示图标 + 文本。
- **Modal 高度**：限制 body 并允许滚动。
- **深色模式**：**当前未实现** — 新 UI 须按浅色主题设计。

---

# Do's and Don'ts

## Do

- 色值以 Figma Color Light Mode 为准；新增语义色先写入全局 CSS 变量再更新本文件。
- 使用语义 CSS 变量（`var(--color-primary)`）而非散落硬编码 hex。
- 使用 `toolbarItems` 的 `type: 'primary'` / `danger: true` 表达操作优先级。
- 改 token 后运行消费方项目的设计校验（如 `pnpm validate:design`）。

## Don't

- 不要引入随机 hex、间距、圆角、阴影 — 复用本文档 token 或 `@zdns/design` theme。
- 不要使用已废弃主色 `#008CFF` 或错误色 `#F5493A` / `#FF4D4F`。
- 不要假设深色模式可用。
- 不要修改 `@zdns/design` / `@zddi/*` 包内源码。
