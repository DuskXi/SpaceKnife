/// <reference types="vite/client" />

/** The help pages' pictures by path below site/src (vite-plugins/help-pictures.ts). */
declare module "virtual:help-pictures" {
  const pictures: Readonly<Record<string, { url: string; width: number; height: number }>>;
  export default pictures;
}
