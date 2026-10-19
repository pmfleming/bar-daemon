/* SPDX-License-Identifier: GPL-2.0-or-later */
/* Exercises the patched production parser and copy paths; no network/services. */
#include "gclue-mozilla.h"
#include "gclue-location.h"

static guint kind (GClueLocation *location)
{
        guint value;
        g_object_get (location, "source-kind", &value, NULL);
        return value;
}

int main (void)
{
        const struct {
                const char *request_kind;
                const char *fallback;
                guint expected;
        } cases[] = {
                { "GeoIP", "", 1 },
                { "WiFi", "", 2 },
                { "3GPP", "", 2 },
                { "3GPP + WiFi", "", 2 },
                { "WiFi", ",\"fallback\":\"ipf\"", 1 },
                { "3GPP + WiFi", ",\"fallback\":\"ipf\"", 1 },
                { "3GPP", ",\"fallback\":\"lacf\"", 2 },
                { "GeoIP", ",\"fallback\":\"unknown\"", 0 },
                { "WiFi", ",\"fallback\":null", 0 },
                { "GeoIP", ",\"fallback\":42", 0 },
                { "unrecognised", "", 0 },
        };
        for (guint i = 0; i < G_N_ELEMENTS (cases); i++) {
                g_autofree char *json = g_strdup_printf (
                        "{\"location\":{\"lat\":53.3,\"lng\":-6.2},\"accuracy\":80000%s}",
                        cases[i].fallback);
                g_autoptr(GError) error = NULL;
                g_autoptr(GClueLocation) fix = gclue_mozilla_parse_response (
                        json, cases[i].request_kind, &error);
                g_assert_no_error (error);
                g_assert_nonnull (fix);
                g_assert_cmpuint (kind (fix), ==, cases[i].expected);
                gclue_location_set_description (fix, "GeoIP");
                g_assert_cmpuint (kind (fix), ==, cases[i].expected);
                g_autoptr(GClueLocation) copy = gclue_location_duplicate (fix);
                g_autoptr(GClueLocation) fresh = gclue_location_duplicate_fresh (fix);
                g_assert_cmpuint (kind (copy), ==, cases[i].expected);
                g_assert_cmpuint (kind (fresh), ==, cases[i].expected);
                g_assert_cmpuint (gclue_location_get_timestamp (copy), ==,
                                  gclue_location_get_timestamp (fix));
        }
        const char *invalid[] = {
                "null", "[]", "{}",
                "{\"location\":null,\"accuracy\":1}",
                "{\"location\":{\"lat\":53,\"lng\":-6}}",
                "{\"location\":{\"lat\":91,\"lng\":-6},\"accuracy\":1}",
                "{\"location\":{\"lat\":53,\"lng\":181},\"accuracy\":1}",
                "{\"location\":{\"lat\":\"53\",\"lng\":-6},\"accuracy\":1}",
                "{\"location\":{\"lat\":53,\"lng\":-6},\"accuracy\":-1}",
        };
        for (guint i = 0; i < G_N_ELEMENTS (invalid); i++) {
                g_autoptr(GError) error = NULL;
                g_autoptr(GClueLocation) fix = gclue_mozilla_parse_response (invalid[i], "GeoIP", &error);
                g_assert_null (fix);
                g_assert_error (error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA);
        }
        g_autoptr(GClueLocation) other = gclue_location_new (53.3, -6.2, 1, "GeoIP");
        g_assert_cmpuint (kind (other), ==, 0);
        return 0;
}
