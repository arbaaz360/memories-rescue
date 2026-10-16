import { createContext } from "preact";
import { useContext } from "preact/hooks";
import type { AppStore } from "./store";

export const StoreContext = createContext<AppStore | null>(null);

export function useStore(): AppStore {
  const store = useContext(StoreContext);
  if (!store) throw new Error("StoreContext missing");
  return store;
}
