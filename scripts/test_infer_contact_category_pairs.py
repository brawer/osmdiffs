# SPDX-FileCopyrightText: 2026 Sascha Brawer <sascha@brawer.ch>
# SPDX-License-Identifier: MIT

from infer_contact_category_pairs import emails, main_category, name_words, phones


def test_main_category():
    assert main_category({"name": "X", "amenity": "cafe", "shop": "bakery"}) == "shop=bakery"
    assert main_category({"shop": "no", "amenity": "cafe"}) == "amenity=cafe"
    assert main_category({"name": "X"}) is None


def test_phones():
    assert phones({"phone": "+41 44 417 77 77; +41 44 417 77 78"}) == {"41444177777", "41444177778"}
    assert phones({"contact:fax": "+41 44 417 77 77"}) == {"41444177777"}
    assert phones({"phone": "044 417 77 77"}) == set()
    assert phones({"name": "+41 44 417 77 77"}) == set()


def test_emails():
    assert emails({"email": "Info@Example.ch, mailto:sales@example.ch?subject=x"}) == {
        "info@example.ch",
        "sales@example.ch",
    }
    assert emails({"contact:email": "x"}) == set()


def test_name_words():
    assert name_words({"name": "Zürich Hauptbahnhof", "brand": "SBB"}) == {"zurich", "hauptbahnhof"}
