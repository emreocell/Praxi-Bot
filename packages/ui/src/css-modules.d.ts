declare module "*.module.css" {
  const classes: { readonly [key: string]: string };
  export default classes;
}

declare module "*.webp" {
  const src: string;
  export default src;
}
