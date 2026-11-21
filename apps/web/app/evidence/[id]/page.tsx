import { useParams, useSearchParams } from "react-router-dom";
import { AppShell } from "../../../components/app-shell";
import { EvidenceView } from "../../../components/sources/evidence-view";
export default function Page() {
 const { id } = useParams<{ id: string }>(); const [query] = useSearchParams();
 return <AppShell active="sources"><EvidenceView id={id!} revision={query.get("revision") ?? undefined} /></AppShell>;
}
