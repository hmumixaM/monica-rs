import { lazy, Suspense, type ComponentType } from "react";
export default function dynamic<P extends object>(load: () => Promise<ComponentType<P>>, options?: { loading?: ComponentType }) {
  const Component = lazy(() => load().then(defaultExport => ({ default: defaultExport })));
  const Loading = options?.loading;
  return function Dynamic(props: P) { return <Suspense fallback={Loading ? <Loading /> : null}><Component {...props} /></Suspense>; };
}
