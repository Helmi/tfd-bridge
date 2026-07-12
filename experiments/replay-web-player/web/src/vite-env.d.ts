/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Set to '1' by the `build:bridge` mode; selects the bridge API adapter over the vite-dev experiment endpoints. */
  readonly VITE_BRIDGE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

