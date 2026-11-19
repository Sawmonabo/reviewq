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
                if url.host_str() != self.origin.host_str()
                    || url.username() != "git"
                    || url.port().is_some_and(|port| port != 22)
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(AppError::NotFound);
                }
                url.path().to_owned()
            }
            _ => {
                let (host, path) = remote.split_once(':').ok_or(AppError::NotFound)?;
                let native = host.strip_prefix("git@").ok_or(AppError::NotFound)?;
                if self.origin.host_str() != Some(native) {
                    return Err(AppError::NotFound);
                }
                path.to_string()
            }
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
}
