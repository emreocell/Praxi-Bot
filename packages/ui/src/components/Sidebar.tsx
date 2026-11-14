import { Slot } from "@radix-ui/react-slot";
import type {
  ButtonHTMLAttributes,
  CSSProperties,
  HTMLAttributes,
  ReactNode
} from "react";
import styles from "./Sidebar.module.css";

export interface SidebarLayoutProps extends HTMLAttributes<HTMLDivElement> {
  sidebar: ReactNode;
  /** Optional full-window backdrop (canvas) drawn between the image layer and the panels. */
  backdrop?: ReactNode;
  /** Optional right-hand panel (a `SidePanel`), laid out after the main pane. */
  aside?: ReactNode;
  children: ReactNode;
}

export function SidebarLayout({
  sidebar,
  backdrop,
  aside,
  children,
  className,
  ...props
}: SidebarLayoutProps) {
  const classes = [styles.layout, className ?? ""].filter(Boolean).join(" ");
  return (
    <div className={classes} {...props}>
      <div className={styles.backgroundImageLayer} aria-hidden="true" />
      {backdrop}
      <div className={styles.backgroundDimLayer} aria-hidden="true" />
      <div className={styles.foregroundContainer}>
        {sidebar}
        <main className={styles.mainPane}>{children}</main>
        {aside}
      </div>
    </div>
  );
}

export type SidebarProps = HTMLAttributes<HTMLElement>;

export function Sidebar({ className, children, ...props }: SidebarProps) {
  const classes = [styles.sidebar, className ?? ""].filter(Boolean).join(" ");
  return (
    <aside className={classes} {...props}>
      {children}
    </aside>
  );
}

export type SidebarHeaderProps = HTMLAttributes<HTMLDivElement>;

export function SidebarHeader({
  className,
  children,
  ...props
}: SidebarHeaderProps) {
  const classes = [styles.header, className ?? ""].filter(Boolean).join(" ");
  return (
    <div className={classes} {...props}>
      {children}
    </div>
  );
}

export interface SidebarSectionProps extends HTMLAttributes<HTMLDivElement> {
  label?: string;
}

export function SidebarSection({
  label,
  className,
  children,
  ...props
}: SidebarSectionProps) {
  const classes = [styles.section, className ?? ""].filter(Boolean).join(" ");
  return (
    <div className={classes} {...props}>
      {label ? <div className={styles.sectionLabel}>{label}</div> : null}
      <div className={styles.sectionList}>{children}</div>
    </div>
  );
}

export interface SidebarItemProps
  extends ButtonHTMLAttributes<HTMLButtonElement> {
  active?: boolean;
  accentColor?: string;
  asChild?: boolean;
}

export function SidebarItem({
  active = false,
  accentColor,
  asChild = false,
  className,
  style,
  type = "button",
  children,
  ...props
}: SidebarItemProps) {
  const Component = asChild ? Slot : "button";
  const classes = [
    styles.item,
    active ? styles.itemActive : "",
    className ?? ""
  ]
    .filter(Boolean)
    .join(" ");

  const mergedStyle: CSSProperties | undefined = accentColor
    ? ({ ...style, "--item-accent": accentColor } as CSSProperties)
    : style;

  return (
    <Component
      type={asChild ? undefined : type}
      aria-current={active ? "page" : undefined}
      className={classes}
      style={mergedStyle}
      {...props}
    >
      {children}
    </Component>
  );
}

export type SidebarFooterProps = HTMLAttributes<HTMLDivElement>;

export function SidebarFooter({
  className,
  children,
  ...props
}: SidebarFooterProps) {
  const classes = [styles.footer, className ?? ""].filter(Boolean).join(" ");
  return (
    <div className={classes} {...props}>
      {children}
    </div>
  );
}

export interface SidePanelProps extends Omit<HTMLAttributes<HTMLElement>, "children"> {
  open: boolean;
  /** Width in px while open. */
  width: number;
  children: ReactNode;
}

/** Right-hand panel: slides open to `width`, inert and out of the tab order while closed. */
export function SidePanel({ open, width, className, style, children, ...props }: SidePanelProps) {
  const classes = [styles.panel, className ?? ""].filter(Boolean).join(" ");
  return (
    <aside
      className={classes}
      data-open={open}
      aria-hidden={!open}
      inert={!open}
      style={{ ...style, "--panel-w": `${width}px` } as CSSProperties}
      {...props}
    >
      <div className={styles.panelInner}>{children}</div>
    </aside>
  );
}
