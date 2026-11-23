use percent_encoding::percent_decode_str;
use reviewq_app::access::AppError;
use reviewq_app::workspace::ConfiguredProject;
use url::Url;

use super::GitLab;

impl GitLab {
    pub(super) async fn verify_checkout_remote(
        &self,
        project: ConfiguredProject,
        remote: String,
    ) -> Result<ConfiguredProject, AppError> {
        if project.origin != self.identity
            || remote.len() > 4096
            || remote.chars().any(char::is_control)
        {
            return Err(AppError::NotFound);
        }
        let path = match Url::parse(&remote) {
            Ok(url) if matches!(url.scheme(), "https" | "http") => {
                if url.origin() != self.origin.origin()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(AppError::NotFound);
                }
                url.path().to_owned()
            }
            Ok(url) if url.scheme() == "ssh" => {
                if url.port().is_some_and(|port| port != 22) {
                    return Err(AppError::NotFound);
                }
                self.ssh_project_path(&url)?.to_owned()
            }
            _ => self.scp_project_path(&remote)?.to_owned(),
        };
        let path = percent_decode_str(&path)
            .decode_utf8()
            .map_err(|_| AppError::InvalidRequest)?;
        let path = path.trim_start_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let expected = self.project(project.project_id).await?;
        if path != expected.path_with_namespace {
            return Err(AppError::NotFound);
        }
        Ok(project)
    }

    pub(super) fn ssh_project_path<'a>(&self, url: &'a Url) -> Result<&'a str, AppError> {
        if url.scheme() != "ssh"
            || !url
                .host_str()
                .zip(self.origin.host_str())
                .is_some_and(|(host, origin)| host.eq_ignore_ascii_case(origin))
            || url.username() != "git"
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AppError::NotFound);
        }
        Ok(url.path())
    }

    pub(super) fn scp_project_path<'a>(&self, remote: &'a str) -> Result<&'a str, AppError> {
        let origin = self.origin.host_str().ok_or(AppError::Internal)?;
        let (host, path) = remote
            .strip_prefix("git@")
            .and_then(|address| address.split_at_checked(origin.len()))
            .ok_or(AppError::NotFound)?;
        if !host.eq_ignore_ascii_case(origin) {
            return Err(AppError::NotFound);
        }
        path.strip_prefix(':').ok_or(AppError::NotFound)
    }
}
