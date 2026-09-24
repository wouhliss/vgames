import { useNavigate } from "react-router";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/Feedback";
import { t } from "../../i18n";
import { Page } from "../Page";

export function LibraryPage() {
  const navigate = useNavigate();
  return (
    <Page title={t("library.title")}>
      <EmptyState
        icon="library"
        title={t("library.emptyTitle")}
        description={t("library.emptyText")}
        action={
          <Button variant="primary" icon="browse" onClick={() => navigate("/browse")}>
            {t("library.emptyAction")}
          </Button>
        }
      />
    </Page>
  );
}
