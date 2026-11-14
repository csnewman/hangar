//! The operations that answer two ways, each read whole: build.rs drops
//! the second answer for progenitor, which takes one success body per
//! operation, and each call here reads it back from the response the
//! generated call did not expect.

use crate::{clientapi, frontendapi};

/// How finishing a sign-in went: the token, shown this once, or the grant
/// still waiting to be approved (202), to be asked again after its
/// interval.
#[derive(Debug, Clone, PartialEq)]
pub enum SignInStep {
    Token(clientapi::types::SignInToken),
    Waiting(clientapi::types::Error),
}

// Each returns progenitor's error, as the generated calls do.
#[allow(clippy::result_large_err)]
impl clientapi::Client {
    /// Collects an approved grant's token, or says it still waits.
    pub async fn finish_sign_in_step(
        &self,
        body: &clientapi::types::FinishSignIn,
    ) -> Result<SignInStep, clientapi::Error<clientapi::types::Error>> {
        match self.finish_sign_in(body).await {
            Ok(token) => Ok(SignInStep::Token(token.into_inner())),
            Err(progenitor_client::Error::UnexpectedResponse(r)) if r.status() == 202 => {
                let waiting = r
                    .json()
                    .await
                    .map_err(progenitor_client::Error::ResponseBodyError)?;
                Ok(SignInStep::Waiting(waiting))
            }
            Err(e) => Err(e),
        }
    }

    /// Deletes an environment: what it is as it goes, or None when it was
    /// gone at once (204).
    pub async fn delete_environment_or_gone(
        &self,
        id: &str,
    ) -> Result<Option<clientapi::types::Environment>, clientapi::Error<clientapi::types::Error>>
    {
        gone(self.delete_environment(id).await)
    }
}

#[allow(clippy::result_large_err)]
impl frontendapi::Client {
    /// Deletes an environment: what it is as it goes, or None when it was
    /// gone at once (204).
    pub async fn delete_environment_or_gone(
        &self,
        id: &str,
    ) -> Result<
        Option<frontendapi::types::Environment>,
        frontendapi::Error<frontendapi::types::Error>,
    > {
        gone(self.delete_environment(id).await)
    }
}

/// A deletion's answer: its body, or None for 204.
#[allow(clippy::result_large_err)]
fn gone<T, E>(
    answer: Result<progenitor_client::ResponseValue<T>, progenitor_client::Error<E>>,
) -> Result<Option<T>, progenitor_client::Error<E>> {
    match answer {
        Ok(v) => Ok(Some(v.into_inner())),
        Err(progenitor_client::Error::UnexpectedResponse(r)) if r.status() == 204 => Ok(None),
        Err(e) => Err(e),
    }
}
