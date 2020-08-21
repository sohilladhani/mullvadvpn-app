import React from 'react';
import styled from 'styled-components';
import { colors } from '../../config.json';
import { buttonText, mediumText, smallText } from './common-styles';
import ImageView, { IImageViewProps } from './ImageView';

export const StyledContainer = styled.div((props: { disabled?: boolean }) => ({
  display: 'flex',
  backgroundColor: colors.blue,
  alignItems: 'center',
  paddingLeft: '22px',
  paddingRight: '16px',
  opacity: props.disabled ? 0.5 : undefined,
}));

export const StyledSection = styled.div({
  display: 'flex',
  flexDirection: 'column',
});

export const StyledSectionTitle = styled.span(buttonText, {
  backgroundColor: colors.blue,
  padding: '14px 16px 14px 22px',
  marginBottom: '1px',
});

interface IStyledCellButtonProps {
  disabled?: boolean;
  selected?: boolean;
  containedInSection: boolean;
}

export const StyledCellButton = styled.button({}, (props: IStyledCellButtonProps) => ({
  display: 'flex',
  padding: '0 16px 0 22px',
  marginBottom: '1px',
  flex: 1,
  alignItems: 'center',
  alignContent: 'center',
  cursor: 'default',
  border: 'none',
  backgroundColor: props.selected
    ? colors.green
    : props.containedInSection
    ? colors.blue40
    : colors.blue,
  ':not(:disabled):hover': {
    backgroundColor: props.selected ? colors.green : colors.blue80,
  },
  opacity: props.disabled ? 0.5 : undefined,
}));

export const StyledLabel = styled.div(buttonText, {
  margin: '14px 0',
  flex: 1,
  color: colors.white,
  textAlign: 'left',
});

export const StyledSubText = styled.span(smallText, {
  color: colors.white60,
  fontWeight: 800,
  flex: -1,
  textAlign: 'right',
  marginLeft: '8px',
  marginRight: '8px',
});

export const StyledTintedIcon = styled(ImageView).attrs((props: IImageViewProps) => ({
  tintColor: props.tintColor ?? colors.white60,
  tintHoverColor: props.tintHoverColor ?? props.tintColor ?? colors.white60,
}))((props: IImageViewProps) => ({
  [StyledCellButton + ':hover &']: {
    backgroundColor: props.tintHoverColor,
  },
}));

export const StyledFooter = styled.div({
  padding: '6px 22px 20px',
});

export const StyledFooterText = styled.span(smallText);

export const StyledFooterBoldText = styled(StyledFooterText)({
  fontWeight: 900,
});

export const StyledInputFrame = styled.div({
  flexGrow: 0,
  backgroundColor: 'rgba(255,255,255,0.1)',
  borderRadius: '4px',
  padding: '4px 8px',
});

const inputTextStyles: React.CSSProperties = {
  ...mediumText,
  fontWeight: 600,
  height: '28px',
  textAlign: 'right',
  padding: '0px',
};

export const StyledInput = styled.input({}, (props: { valid?: boolean }) => ({
  ...inputTextStyles,
  backgroundColor: 'transparent',
  border: 'none',
  width: '100%',
  height: '100%',
  color: props.valid !== false ? colors.white : colors.red,
  '::placeholder': {
    color: colors.white60,
  },
}));

export const StyledAutoSizingTextInputContainer = styled.div({
  position: 'relative',
});

export const StyledAutoSizingTextInputFiller = styled.pre({
  ...inputTextStyles,
  minWidth: '80px',
  color: 'transparent',
});

export const StyledAutoSizingTextInputWrapper = styled.div({
  position: 'absolute',
  top: '0px',
  left: '0px',
  width: '100%',
  height: '100%',
});
