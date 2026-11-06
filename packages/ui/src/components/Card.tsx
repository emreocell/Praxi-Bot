import { Slot } from "@radix-ui/react-slot";
import type { CSSProperties, ForwardedRef, HTMLAttributes } from "react";
import { forwardRef } from "react";
import styles from "./Card.module.css";

export type CardPadding = "none" | "sm" | "md" | "lg";

export interface CardProps extends HTMLAttributes<HTMLDivElement> {
  padding?: CardPadding;
  elevated?: boolean;
  accentColor?: string;
  asChild?: boolean;
}

export const Card = forwardRef(function Card(
  {
    padding = "md",
    elevated = false,
    accentColor,
    asChild = false,
    className,
    style,
    ...props
  }: CardProps,
  ref: ForwardedRef<HTMLDivElement>
) {
  const Component = asChild ? Slot : "div";
  const classes = [
    styles.card,
    styles[`padding-${padding}`],
    elevated ? styles.elevated : "",
    accentColor ? styles.accented : "",
    className ?? ""
  ]
    .filter(Boolean)
    .join(" ");

  const mergedStyle: CSSProperties | undefined = accentColor
    ? ({ ...style, "--card-accent": accentColor } as CSSProperties)
    : style;

  return (
    <Component ref={ref} className={classes} style={mergedStyle} {...props} />
  );
});
