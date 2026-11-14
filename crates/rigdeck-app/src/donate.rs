//! Donation links shown in the sidebar's "Support RigDeck" section.

use eframe::egui;

/// PayPal payment page set up by the author (a $10 payment).
pub const PAYPAL_URL: &str = "https://www.paypal.com/ncp/payment/PLB-6CEULH5Q3URL";
/// PayPal account, for sending another amount from the PayPal app or website.
pub const PAYPAL_ACCOUNT: &str = "ortizl@gmail.com";
/// Cash App $Cashtag.
pub const CASHTAG: &str = "$lortiz1721";

/// Cash App's public payment page for the $Cashtag.
pub fn cash_app_url() -> String {
    format!("https://cash.app/{CASHTAG}")
}

pub const WHY: &str = "RigDeck is free and open source (MIT licence), with no ads, no account and no subscription. \
It is built by a ham for the ham radio community.";

pub const WHAT_IT_PAYS_FOR: &str = "Donations pay for further development of the app: support for more radios, \
new features, fixes, and the time spent testing on real radios. The code stays free for every amateur.";

pub const THANKS: &str = "Any amount helps. Thank you, and 73!";

pub fn section(ui: &mut egui::Ui) {
    ui.label(WHY);
    ui.label(WHAT_IT_PAYS_FOR);
    ui.horizontal(|ui| {
        if ui.button("Donate with PayPal").on_hover_text(PAYPAL_URL).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(PAYPAL_URL));
        }
        let cash = cash_app_url();
        if ui.button("Donate with Cash App").on_hover_text(&cash).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(cash));
        }
    });
    let weak = ui.visuals().weak_text_color();
    let small = |t: String| egui::RichText::new(t).size(11.0).color(weak);
    ui.label(small(format!(
        "The PayPal button opens a $10 payment. For another amount, send it in PayPal to {PAYPAL_ACCOUNT}. Cash App: {CASHTAG}."
    )));
    ui.label(THANKS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donation_links_point_at_paypal_and_cash_app() {
        assert_eq!(cash_app_url(), "https://cash.app/$lortiz1721");
        assert!(PAYPAL_URL.starts_with("https://www.paypal.com/"));
    }
}
