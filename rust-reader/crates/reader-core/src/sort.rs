use std::cmp::Ordering;

/// Compares names the way a reader expects numbered pages to appear.
///
/// ASCII digit runs are compared as numbers first, so `2.png` comes before
/// `10.png`. Leading zeros only break otherwise equal runs, and the final
/// comparison is byte-stable for deterministic indexes.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left_lower = left.to_ascii_lowercase();
    let right_lower = right.to_ascii_lowercase();
    let left_bytes = left_lower.as_bytes();
    let right_bytes = right_lower.as_bytes();
    let mut left_index = 0usize;
    let mut right_index = 0usize;

    while left_index < left_bytes.len() && right_index < right_bytes.len() {
        let left_digit = left_bytes[left_index].is_ascii_digit();
        let right_digit = right_bytes[right_index].is_ascii_digit();

        if left_digit && right_digit {
            let left_end = digit_run_end(left_bytes, left_index);
            let right_end = digit_run_end(right_bytes, right_index);
            let left_significant = first_significant_digit(left_bytes, left_index, left_end);
            let right_significant = first_significant_digit(right_bytes, right_index, right_end);
            let left_number_len = left_end - left_significant;
            let right_number_len = right_end - right_significant;

            match left_number_len.cmp(&right_number_len) {
                Ordering::Equal => {}
                other => return other,
            }

            match left_lower[left_significant..left_end]
                .cmp(&right_lower[right_significant..right_end])
            {
                Ordering::Equal => {}
                other => return other,
            }

            match (left_end - left_index).cmp(&(right_end - right_index)) {
                Ordering::Equal => {}
                other => return other,
            }

            left_index = left_end;
            right_index = right_end;
            continue;
        }

        match left_bytes[left_index].cmp(&right_bytes[right_index]) {
            Ordering::Equal => {
                left_index += 1;
                right_index += 1;
            }
            other => return other,
        }
    }

    match left_bytes.len().cmp(&right_bytes.len()) {
        Ordering::Equal => left.cmp(right),
        other => other,
    }
}

fn digit_run_end(value: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < value.len() && value[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn first_significant_digit(value: &[u8], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end - 1 && value[index] == b'0' {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn unpadded_numbers_sort_before_larger_numbers() {
        let mut names = vec!["10.png", "2.png", "1.png", "page10.png", "page2.png"];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(
            names,
            vec!["1.png", "2.png", "10.png", "page2.png", "page10.png"]
        );
    }

    #[test]
    fn leading_zero_runs_are_stable() {
        assert_eq!(natural_cmp("01.png", "1.png"), Ordering::Greater);
        assert_eq!(natural_cmp("001.png", "01.png"), Ordering::Greater);
        assert_eq!(natural_cmp("a02/b.png", "a2/c.png"), Ordering::Greater);
    }
}
