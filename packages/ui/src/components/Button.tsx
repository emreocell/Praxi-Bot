import { Slot } from "@radix-ui/react-slot";
import type { ButtonHTMLAttributes, ForwardedRef } from "react";
import { forwardRef } from "react";
import styles from "./Button.module.css";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  active?: boolean;
  fullWidth?: boolean;
  asChild?: boolean;
}

export const Button = forwardRef(function Button(
  {
    variant = "secondary",
    size = "md",
    active = false,
    fullWidth = false,
    asChild = false,
    className,
    type = "button",
    ...props
  }: ButtonProps,
  ref: ForwardedRef<HTMLButtonElement>
) {
  const Component = asChild ? Slot : "button";
  const classes = [
    styles.button,
    styles[variant],
    styles[size],
    active ? styles.active : "",
    fullWidth ? styles.fullWidth : "",
    className ?? ""
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <Component
      ref={ref}
      type={asChild ? undefined : type}
      className={classes}
      {...props}
    />
  );
});
