// Image imports resolve to their bundled URL (Bun bundler).
declare module "*.png" {
  const url: string;
  export default url;
}
