/// <reference types="vite/client" />

declare module "*.svg?raw" {
  const content: string;
  export default content;
}

declare module "highlight.js/styles/*.css" {
  const _: void;
  export default _;
}
